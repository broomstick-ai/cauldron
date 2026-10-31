//! **MIC MODE'S THREE LEGS — capture, transcription, playback — and the GATE
//! that stands in front of the middle one.**
//!
//! # Read this first: exit 0 is not a validity signal
//!
//! `whisper` returns the word **`you`**, at **exit 0**, on digital silence.
//! That is measured on this machine, on both `ggml-small.en` and
//! `ggml-large-v3-turbo`, by the scout (`0055`) and again by this build before
//! a line of the gate was written:
//!
//! ```text
//! $ retranscribe silence.wav   # 1.0s of 16 kHz mono Int16 zeros
//! exit=0  stdout=[you]
//! ```
//!
//! So an ungranted, blocked or dead microphone **does not error**. It does not
//! return an empty string and it does not return a non-zero exit. It returns a
//! plausible one-word English transcript that this face would otherwise show
//! him as his own speech. And the most likely microphone-permission failure on
//! macOS is not a dialog and not an error — it is an OPEN STREAM DELIVERING
//! SILENCE, which lands in exactly that hole.
//!
//! [`transcribe`] therefore refuses on PEAK, before the transcriber is
//! invoked at all. The threshold and the idea are copied from the AUI's
//! production defence (`apps/aui/src/stt.rs:99` `DEAD_STREAM_PEAK`, applied at
//! `:225`), whose own comment names the hallucinated fillers — `"you"`,
//! `"Yeah"`, `"Thank you."` — and calls the resulting bug *"the exact 'my long
//! input became one word' failure"*. `retranscribe` deliberately does NOT
//! carry this gate (its warning is advisory and leaves stdout and the exit code
//! untouched), so the gate is ours.
//!
//! **The gate is proven by being forced to fail**, against a real silent WAV,
//! in this module's own tests and again at app level.
//!
//! # The commands are INJECTABLE, and that is dependency injection, not a stub
//!
//! Every leg is an external command, and each one can be replaced through an
//! environment variable ([`Cmd::from_env`]). The real path is `rec`,
//! `retranscribe` and `afplay`; the test path substitutes a script that copies
//! a fixture WAV into place.
//!
//! **This is a boundary substitution, not a fake implementation.** No stub
//! pretends to work: with nothing injected, mic mode runs the real binaries and
//! there is no code path that invents a transcript or synthesises audio. What
//! the injection buys is that the whole pipeline — gate, transcription,
//! failure handling, queue rows, send — is provable **without opening the
//! microphone**, which this build is forbidden to do: acquiring the input
//! device reconfigures the audio route and invalidates the AUI's already-open
//! output handle (`apps/aui/src/dispatch/thread.rs:3875-3884`), and the
//! operator is on that live audio interface.
//!
//! **The live-device leg is consequently UNPROVEN and is reported as
//! unproven.** A one-time microphone grant in System Settings is owed from the
//! operator and no code substitutes for it.
//!
//! # Why `rec` and not `cpal`
//!
//! Permission durability, measured rather than preferred. `target/debug/tui` is
//! adhoc, linker-signed, with `Info.plist=not bound`, and cargo replaces that
//! path and its cdhash on every build — so macOS TCC can key it only by PATH,
//! and the path's contents change constantly. Under tmux there is no
//! responsible GUI app either (the tmux server is PPID 1, re-parented to
//! launchd), so macOS may be unable to present a permission prompt at all.
//! `/opt/homebrew/bin/rec` is a stable path — it has pointed at
//! `Cellar/sox/14.4.2_6` since March 2025 and is not rebuilt when `tui` is — so
//! a consent row attached to it survives every rebuild. It also keeps `cpal`
//! out of a crate that declares `unsafe_code = "forbid"`.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use std::fs;

/// The directory mode for `~/.tui/captures`. `0700`, matching [`crate::draft`]:
/// nobody else's business what he said into his own machine.
const DIR_MODE: u32 = 0o700;
/// The file mode for a capture. `0600`. **His voice is at least as private as
/// his draft**, and the draft journal's argument applies here verbatim.
const FILE_MODE: u32 = 0o600;

/// **THE DEAD-AUDIO THRESHOLD.** Peak sample amplitude at or below this and the
/// capture is treated as a dead input stream: the hardware sent no signal.
///
/// The number and the reasoning are the AUI's (`apps/aui/src/stt.rs:99`), taken
/// rather than re-derived, because it is the value that has been holding this
/// exact line in production. Anything above it — including quiet-but-real
/// speech — is real audio and goes to the transcriber.
pub const DEAD_PEAK: i32 = 10;

/// 16 kHz mono signed 16-bit PCM — what the AUI captures, what `whisper` wants,
/// and what `afplay` plays. The three legs were already designed to meet.
pub const SAMPLE_RATE: u32 = 16_000;

// ─────────────────────────────────────────────────────────────────────────────
//  THE WAV READING — hand-rolled, and the argument for that
// ─────────────────────────────────────────────────────────────────────────────

/// Where a WAV's sample data starts and how many bytes of it are really there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataChunk {
    /// Byte offset of the first sample.
    pub start: usize,
    /// What the header CLAIMS the data length is.
    pub declared: usize,
    /// What is actually behind that offset on disk.
    pub available: usize,
    pub bits_per_sample: u16,
}

impl DataChunk {
    /// **The bytes worth reading — what is THERE, not what is claimed.**
    ///
    /// A writer killed mid-capture leaves a WAV declaring zero bytes with real
    /// speech behind it. Measuring through the declared length calls that file
    /// silent, which would route a real utterance into the dead-audio refusal —
    /// the same confusion the AUI had to fix at `stt.rs`'s
    /// `wav_peak_amplitude`, and for the same reason.
    pub fn usable(&self) -> usize {
        if self.declared == 0 || self.declared > self.available {
            self.available
        } else {
            self.declared
        }
    }

    /// Whether the header tells the truth about what follows it.
    pub fn declares_the_truth(&self) -> bool {
        self.declared != 0 && self.declared <= self.available
    }
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// Walk a RIFF/WAVE header and locate the `data` chunk.
///
/// **Hand-rolled rather than taken from `hound`, deliberately.** `hound` is in
/// the registry and the AUI uses it, but adding a dependency to `apps/tui`
/// rewrites the WORKSPACE-ROOT `Cargo.lock`, which is shared build
/// infrastructure this task does not own (the same boundary `0042` declined to
/// cross for a crossterm fork). The parse it would have saved is fifty lines of
/// safe integer reads, and owning it is what lets [`DataChunk::usable`] measure
/// the disk instead of the header — the behaviour that matters most here and
/// the one a library would have hidden.
pub fn parse_wav(bytes: &[u8]) -> Option<DataChunk> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12usize;
    let mut bits: Option<u16> = None;
    while at + 8 <= bytes.len() {
        let id = bytes.get(at..at + 4)?;
        let size = u32_le(bytes, at + 4)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            // `bits_per_sample` is at offset 14 of the fmt body.
            bits = u16_le(bytes, body + 14);
        } else if id == b"data" {
            return Some(DataChunk {
                start: body,
                declared: size,
                available: bytes.len().saturating_sub(body),
                // 16 is the only width this face produces; if `fmt ` was
                // unreadable we report what we saw rather than guessing.
                bits_per_sample: bits.unwrap_or(0),
            });
        }
        // Chunks are word-aligned: an odd size carries a pad byte.
        let step = 8 + size + (size & 1);
        // A zero-size chunk that is not `data` would spin forever.
        if step <= 8 && id != b"data" {
            at = body;
            continue;
        }
        at = at.checked_add(step)?;
    }
    None
}

/// **PEAK SAMPLE AMPLITUDE, over the audio that is actually on disk.**
///
/// `None` when the bytes cannot be read as 16-bit PCM — and that is a
/// deliberate refusal to answer rather than a zero. **We never accuse a device
/// of being dead on a mere read error**: a file we cannot parse is a different
/// failure from a microphone that sent nothing, and collapsing the two would
/// make the gate's message a lie in one of the two cases.
pub fn wav_peak(bytes: &[u8]) -> Option<i32> {
    let d = parse_wav(bytes)?;
    if d.bits_per_sample != 16 {
        return None;
    }
    let n = d.usable();
    let end = d.start.checked_add(n)?;
    let samples = bytes.get(d.start..end.min(bytes.len()))?;
    if samples.len() < 2 {
        return None;
    }
    let mut peak: i32 = 0;
    for pair in samples.chunks_exact(2) {
        let v = i32::from(i16::from_le_bytes([pair[0], pair[1]])).abs();
        if v > peak {
            peak = v;
        }
    }
    Some(peak)
}

/// **REPAIR A HEADER THAT UNDER-DECLARES ITS OWN AUDIO**, in place.
///
/// Returns `Ok(true)` when it rewrote something.
///
/// This exists because a capture severed mid-recording is otherwise SILENTLY
/// LOST, and that is measured rather than assumed. A WAV holding real speech
/// whose `data` chunk declares zero — the signature a killed writer leaves —
/// was fed to both downstream legs on this machine:
///
/// ```text
/// retranscribe unfinalized.wav  ->  exit 3, stdout empty ("EMPTY TRANSCRIPT")
/// afplay       unfinalized.wav  ->  exit 1, "AudioFileOpen failed ('dta?')"
/// ```
///
/// `retranscribe` does not repair headers (the AUI has a whole module for it,
/// `apps/aui/src/wav_repair.rs`), so without this his words would come back as
/// a transcription failure over a file that had his voice in it the whole time.
/// Correctness at the source: repair the capture, do not teach the reader to
/// cope with a broken one.
pub fn repair_header(path: &Path) -> std::io::Result<bool> {
    let mut head = vec![0u8; 4096];
    let mut f = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let total = f.metadata()?.len() as usize;
    let n = f.read(&mut head)?;
    head.truncate(n);
    let Some(d) = parse_wav(&head) else {
        return Ok(false);
    };
    let available = total.saturating_sub(d.start);
    if d.declared != 0 && d.declared <= available {
        return Ok(false); // the header is honest; leave it alone
    }
    if available == 0 {
        return Ok(false); // nothing to point at
    }
    // `data` size, then the RIFF size (total - 8).
    f.seek(SeekFrom::Start((d.start - 4) as u64))?;
    f.write_all(&(available as u32).to_le_bytes())?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&((total - 8) as u32).to_le_bytes())?;
    f.flush()?;
    Ok(true)
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE INJECTABLE COMMAND BOUNDARY
// ─────────────────────────────────────────────────────────────────────────────

/// An external command, with `{}` standing for the audio file.
///
/// Parsed on whitespace and run WITHOUT a shell — there is no `sh -c` anywhere
/// in this module, so nothing in a path or an override can be word-split into
/// an extra argument or interpreted as a redirection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    /// Parse `"prog a b {}"`. `None` when there is no program word.
    pub fn parse(s: &str) -> Option<Cmd> {
        let mut it = s.split_whitespace();
        let program = it.next()?.to_string();
        Some(Cmd {
            program,
            args: it.map(str::to_string).collect(),
        })
    }

    /// The override in `var`, or `fallback` parsed.
    pub fn from_env(var: &str, fallback: &str) -> Option<Cmd> {
        match std::env::var(var) {
            Ok(s) if !s.trim().is_empty() => Cmd::parse(&s),
            _ => Cmd::parse(fallback),
        }
    }

    /// This command with `{}` replaced by `path`.
    pub fn resolved(&self, path: &Path) -> (String, Vec<String>) {
        let p = path.to_string_lossy().to_string();
        let args = self.args.iter().map(|a| a.replace("{}", &p)).collect();
        (self.program.clone(), args)
    }

    fn command(&self, path: &Path) -> Command {
        let (prog, args) = self.resolved(path);
        let mut c = Command::new(prog);
        c.args(args);
        c
    }
}

/// `rec -r 16000 -c 1 -b 16 -e signed-integer <out>`, the spec the scout
/// validated end-to-end (its output half was pushed through `sox` on a file and
/// read by `retranscribe` at exit 0 and `afplay` at exit 0, without the device).
pub const CAPTURE_DEFAULT: &str =
    "/opt/homebrew/bin/rec -q -r 16000 -c 1 -b 16 -e signed-integer {}";
/// Overrides the capture command. **The test seam.**
pub const CAPTURE_VAR: &str = "TUI_MIC_CAPTURE";

/// Overrides the transcriber. Default is resolved against `$HOME` at call time.
pub const STT_VAR: &str = "TUI_MIC_STT";
/// **THE MODEL, as one obvious pass-through.** Set it to a `ggml-*.bin` path
/// and it reaches the transcriber as `RETRANSCRIBE_MODEL`.
///
/// It is now an OVERRIDE and no longer the only way to get the large model —
/// see [`model_path`] for the default and for the ruling that changed it.
pub const MODEL_VAR: &str = "TUI_MIC_MODEL";

/// The file name of the model both mic gestures now run.
pub const LARGE_MODEL_FILE: &str = "ggml-large-v3-turbo.bin";

/// **WHICH MODEL BOTH GESTURES RUN — `large-v3-turbo`, BY DEFAULT, AS OF
/// `IS-53`'s CLOSURE.**
///
/// This module used to ship `small.en` by silence: it set `RETRANSCRIBE_MODEL`
/// only when [`MODEL_VAR`] was set, and `retranscribe` with nothing to go on
/// picks the largest `ggml-*.bin` under `~/.aui/models`, which on this machine
/// holds exactly one file and it is `ggml-small.en.bin`. That default was
/// DELIBERATE and it was argued for responsiveness — 394–433 ms against
/// 1344–1567 ms per invocation, measured by `0095`, roughly one extra second on
/// the "transcribing…" row every time.
///
/// **He priced it and he bought it:** *"one second is fine. Need whisper
/// large."* `IS-53` is closed on those words and it rules BOTH paths — the
/// held-space gesture this round adds and the tap-toggle that was already here
/// — so the model is no longer a lever he has to remember to pull, and the two
/// halves of one feature can no longer disagree about how well they hear him.
///
/// The ladder is the live AUI's own (`apps/aui/src/models.rs`), minus the
/// download: an explicit [`MODEL_VAR`], then `~/.aui/models`, then
/// `~/code/whisper.cpp/models` — which is where the AUI actually resolves it
/// today, because the first directory does not carry it.
///
/// **`None` is not a failure and does not pretend to be one.** It means no
/// large model is on this machine, and the transcriber is then left to its own
/// resolution exactly as before rather than being handed a path that is not
/// there. An explicit [`MODEL_VAR`] is passed through UNVALIDATED: second-
/// guessing a path he typed would be this module deciding it knows better.
pub fn model_path() -> Option<PathBuf> {
    if let Ok(m) = std::env::var(MODEL_VAR) {
        let m = m.trim();
        if !m.is_empty() {
            return Some(PathBuf::from(m));
        }
    }
    let home = dirs::home_dir()?;
    [
        home.join(".aui").join("models").join(LARGE_MODEL_FILE),
        home.join("code")
            .join("whisper.cpp")
            .join("models")
            .join(LARGE_MODEL_FILE),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// Overrides playback. Default `afplay`.
pub const PLAY_VAR: &str = "TUI_MIC_PLAY";
pub const PLAY_DEFAULT: &str = "/usr/bin/afplay {}";

fn stt_default() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    format!(
        "{} {{}}",
        home.join("plugins/core/tools/retranscribe").display()
    )
}

pub fn capture_cmd() -> Option<Cmd> {
    Cmd::from_env(CAPTURE_VAR, CAPTURE_DEFAULT)
}

pub fn stt_cmd() -> Option<Cmd> {
    Cmd::from_env(STT_VAR, &stt_default())
}

pub fn play_cmd() -> Option<Cmd> {
    Cmd::from_env(PLAY_VAR, PLAY_DEFAULT)
}

// ─────────────────────────────────────────────────────────────────────────────
//  WHERE CAPTURES LIVE
// ─────────────────────────────────────────────────────────────────────────────

/// `~/.tui/captures`. `None` when there is no home directory, which is a legal
/// state: mic mode then simply has nowhere to record to and says so.
pub fn dir() -> Option<PathBuf> {
    // `~` is `face_home`: an isolated process records into scratch (REL-6a4).
    crate::cauldron::live::face_home().map(|h| h.join(".tui").join("captures"))
}

/// Create the capture directory at `0700` and return a fresh `0600` file path
/// inside it.
///
/// **Both modes are set at CREATE**, exactly as [`crate::draft`] does it and for
/// the same reason: a `chmod` afterwards leaves a window in which the file
/// exists at the process umask, and on this path the content of that window is
/// his voice.
pub fn new_capture_path() -> std::io::Result<PathBuf> {
    let d = dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to record into",
        )
    })?;
    fs::create_dir_all(&d)?;
    fs::set_permissions(&d, fs::Permissions::from_mode(DIR_MODE))?;
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|x| x.as_nanos())
        .unwrap_or(0);
    let p = d.join(format!("cap-{}-{}.wav", std::process::id(), stamp));
    // Create it now, at the mode we want, so the recorder inherits an existing
    // private file rather than making a world-readable one at the umask.
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&p)?;
    Ok(p)
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE CONTENTION REFUSAL
// ─────────────────────────────────────────────────────────────────────────────

/// How recently the AUI must have touched a capture for us to call it live.
const AUI_CAPTURE_FRESH: Duration = Duration::from_secs(3);

/// **IS THE AUDIO INTERFACE CAPTURING RIGHT NOW?**
///
/// Acquiring the microphone RECONFIGURES THE AUDIO ROUTE and invalidates the
/// AUI's already-open output handle (`apps/aui/src/dispatch/thread.rs:
/// 3875-3884`). CoreAudio will not refuse a second opener — there is no hog
/// mode anywhere in `apps/aui/src` — and the AUI's own "capture already active"
/// guard (`audio.rs:154-156`) is IN-PROCESS and cannot see a separate `tui`.
/// So nothing stops this face from talking over his live session except this.
///
/// **This is a BEST-EFFORT DETECTOR AND NOT A LOCK, and it is labelled that way
/// on purpose.** It watches for a capture file in the AUI's own capture store
/// that has been written within the last few seconds, which is the only
/// evidence of the AUI's state that is visible from outside its process. It can
/// miss: a capture opened in the instant between the probe and our spawn is not
/// seen, and the AUI could hold the device without writing. **A real guard
/// needs a cross-process lease that one of the two processes must write, and
/// the AUI does not write one** — adding it is a change to `apps/aui`, which
/// this task does not own. What ships is therefore an HONEST VISIBLE REFUSAL in
/// the common case rather than a silent race in every case, and the residual
/// race is reported rather than papered over.
pub fn aui_is_capturing_in(dir: &Path, now: SystemTime) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        if e.path().extension().is_none_or(|x| x != "wav") {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        let Ok(mt) = m.modified() else { continue };
        if now
            .duration_since(mt)
            .map(|d| d < AUI_CAPTURE_FRESH)
            .unwrap_or(true)
        {
            return true;
        }
    }
    false
}

/// [`aui_is_capturing_in`] against the AUI's real capture store.
pub fn aui_is_capturing() -> bool {
    let Some(h) = dirs::home_dir() else {
        return false;
    };
    aui_is_capturing_in(&h.join(".aui").join("captures"), SystemTime::now())
}

// ─────────────────────────────────────────────────────────────────────────────
//  CAPTURE
// ─────────────────────────────────────────────────────────────────────────────

/// A recording in flight.
pub struct Recording {
    child: Child,
    pub path: PathBuf,
}

/// Why a recording could not start.
#[derive(Debug)]
pub enum StartError {
    /// The audio interface is capturing. See [`aui_is_capturing`].
    AuiBusy,
    /// No capture command could be resolved.
    NoCommand,
    Io(std::io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AuiBusy => write!(
                f,
                "the audio interface is using the microphone — mic mode did not open it"
            ),
            StartError::NoCommand => write!(f, "no capture command"),
            StartError::Io(e) => write!(f, "could not start recording: {e}"),
        }
    }
}

impl Recording {
    /// **START RECORDING.** Refuses while the AUI is capturing.
    pub fn start() -> Result<Recording, StartError> {
        if aui_is_capturing() {
            return Err(StartError::AuiBusy);
        }
        let cmd = capture_cmd().ok_or(StartError::NoCommand)?;
        let path = new_capture_path().map_err(StartError::Io)?;
        let child = cmd
            .command(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // sox is chatty on stderr and this process owns a raw-mode
            // terminal: a stray write from a child would garble the screen,
            // which `0018` names as a defect class of its own.
            .stderr(Stdio::null())
            .spawn()
            .map_err(StartError::Io)?;
        Ok(Recording { child, path })
    }

    /// **STOP, AND GET A FILE THAT THE OTHER TWO LEGS CAN READ.**
    ///
    /// `SIGINT` first, so `sox` finalizes its own header, then a bounded wait,
    /// then `SIGKILL` as the backstop — and [`repair_header`] over whatever
    /// results, because a killed writer leaves a WAV declaring zero bytes with
    /// real speech behind it and neither `retranscribe` nor `afplay` will read
    /// one (both measured; see [`repair_header`]).
    ///
    /// The signal is sent by spawning `/bin/kill` rather than calling `libc`,
    /// because this crate declares `unsafe_code = "forbid"` and an FFI call
    /// would need it. It is the same syscall by a longer road.
    pub fn stop(mut self) -> PathBuf {
        let pid = self.child.id();
        let _ = Command::new("/bin/kill")
            .arg("-INT")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = std::time::Instant::now() + Duration::from_millis(1500);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if let Err(e) = repair_header(&self.path) {
            tracing::debug!(error = %e, "could not inspect the capture header");
        }
        self.path
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  TRANSCRIPTION, AND THE GATE
// ─────────────────────────────────────────────────────────────────────────────

/// Why a capture did not become text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttError {
    /// **THE GATE FIRED.** The audio is silent: peak at or below [`DEAD_PEAK`].
    /// The transcriber was NOT invoked.
    DeadAudio {
        peak: i32,
    },
    /// The capture has no readable audio in it at all — a zero-byte file, or
    /// bytes that are not a 16-bit WAV. Distinct from [`SttError::DeadAudio`]
    /// because "I could not read this" and "the microphone sent nothing" are
    /// different failures and only one of them accuses the hardware.
    NoAudio,
    /// The transcriber ran and refused. Carries its exit code, whose meaning is
    /// `retranscribe`'s documented contract: 1 unreadable, 2 no engine or no
    /// model, 3 empty transcript.
    Engine {
        code: i32,
    },
    /// It exited 0 and said nothing.
    Empty,
    /// It could not be run at all.
    Spawn(String),
    NoCommand,
}

impl SttError {
    /// **WHAT THE QUEUE ROW SAYS**, in his language and never in an exit code.
    pub fn plainly(&self) -> String {
        match self {
            SttError::DeadAudio { .. } => {
                "heard nothing — the microphone sent silence, so nothing was transcribed".into()
            }
            SttError::NoAudio => "the recording has no audio in it".into(),
            SttError::Engine { code: 2 } => "no transcription engine or model available".into(),
            SttError::Engine { code: 1 } => "the recording could not be read".into(),
            SttError::Engine { .. } => "nothing could be made out of the recording".into(),
            SttError::Empty => "the transcript came back empty".into(),
            SttError::Spawn(e) => format!("could not run the transcriber: {e}"),
            SttError::NoCommand => "no transcriber configured".into(),
        }
    }
}

/// **THE GATE, AS A PURE FUNCTION OVER BYTES**, so it can be proven without a
/// filesystem and without a transcriber.
///
/// `Ok(peak)` means the audio is real. `Err` means do not transcribe it.
pub fn gate(bytes: &[u8]) -> Result<i32, SttError> {
    match wav_peak(bytes) {
        None => Err(SttError::NoAudio),
        Some(p) if p <= DEAD_PEAK => Err(SttError::DeadAudio { peak: p }),
        Some(p) => Ok(p),
    }
}

/// **TRANSCRIBE A CAPTURE — GATE FIRST, ALWAYS.**
///
/// The order is the whole point and it is not an optimisation: on a silent
/// capture the transcriber returns `you` at exit 0, so asking it first and
/// checking afterwards would mean the fabricated word had already been
/// produced. The gate refuses before the process is spawned.
pub fn transcribe(path: &Path) -> Result<String, SttError> {
    let bytes = fs::read(path).map_err(|_| SttError::NoAudio)?;
    let peak = gate(&bytes)?;
    tracing::debug!(peak, path = %path.display(), "capture passed the dead-audio gate");

    let cmd = stt_cmd().ok_or(SttError::NoCommand)?;
    let mut c = cmd.command(path);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    match model_path() {
        Some(m) => {
            c.env("RETRANSCRIBE_MODEL", &m);
        }
        // Said out loud in the log rather than silently downgraded: the
        // transcriber will still answer, with whatever IT resolves, and that is
        // a different model from the one `IS-53` rules.
        None => tracing::warn!(
            model = LARGE_MODEL_FILE,
            "no large model on this machine — the transcriber will pick its own"
        ),
    }
    let out = c.output().map_err(|e| SttError::Spawn(e.to_string()))?;
    if !out.status.success() {
        return Err(SttError::Engine {
            code: out.status.code().unwrap_or(-1),
        });
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Err(SttError::Empty);
    }
    Ok(text)
}

// ─────────────────────────────────────────────────────────────────────────────
//  PLAYBACK
// ─────────────────────────────────────────────────────────────────────────────

/// **PLAY A CAPTURE, DETACHED.**
///
/// `afplay` BLOCKS FOR THE FULL DURATION OF THE FILE — measured: a 4.77 s clip
/// took 5.39 s of wall time. `.status()` or `.output()` here would freeze the
/// face for the length of every clip, which in a terminal reads as a hang. So
/// it is spawned and the child is RETAINED by the caller, which is also what
/// lets a second press stop or replace the playback.
///
/// A missing file is exit 1 (`AudioFileOpen failed ('wht?')`) — the child
/// reports it and the caller reaps it; see [`reap`].
pub fn play(path: &Path) -> Result<Child, String> {
    let cmd = play_cmd().ok_or_else(|| "no playback command".to_string())?;
    cmd.command(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}

/// Stop a playback that is still running. Safe on one that already finished.
pub fn stop_playing(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// **HAS PLAYBACK FINISHED, AND DID IT WORK?**
///
/// `None` while it is still playing. `Some(Ok(()))` on a clean finish;
/// `Some(Err(code))` when `afplay` refused the file — which is the case the
/// play-button failure test forces, by pointing it at a capture that is gone.
pub fn reap(child: &mut Child) -> Option<Result<(), i32>> {
    match child.try_wait() {
        Ok(Some(s)) if s.success() => Some(Ok(())),
        Ok(Some(s)) => Some(Err(s.code().unwrap_or(-1))),
        Ok(None) => None,
        Err(_) => Some(Err(-1)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE SECOND GESTURE — HOLD TO SPEAK
// ─────────────────────────────────────────────────────────────────────────────
//
// **HIS ASK:** *"it would be useful to have the option to STT stream…
// the way claude code does it is if you hold down space then it does a STT
// stream… in addition to the toggle mode."*
//
// **THERE IS NO KEY-RELEASE EVENT IN HIS TERMINAL AND CLAUDE CODE DOES NOT USE
// ONE EITHER.** Measured by `0095` on a private tmux configured exactly as his
// live server: tmux 3.5a never forwards the kitty keyboard push or query to
// iTerm2 (0 outward occurrences across four negotiations; a DCS-passthrough
// positive control returned 1, so the negative is evidence and not silence).
// The only extended-key channel that survives his tmux is xterm
// `modifyOtherKeys`, which has no release concept at all. And the shipped
// Claude Code binary pushes kitty flags `>1u`/`>5u` — NEITHER contains bit 2,
// `REPORT_EVENT_TYPES` — and its handler is `handleKeyDown`. It counts
// AUTO-REPEAT and ends the gesture on an IDLE GAP.
//
// **So does this.** It is the same rule he already ruled on for the
// unterminated paste — *"if you detect raw characters after the paste starts,
// and then all of a sudden there's a gap of 1s since the last character, it's
// probably done"* — and [`crate::vt::PASTE_IDLE`] is its sibling.
//
// **WHERE IT LIVES, AND WHY NOT IN `vt`.** `0095` recommended the threshold sit
// beside `PASTE_IDLE` in `vt.rs`. It does not, deliberately: `vt` is a BYTE
// parser that knows nothing of modes or acts, and this machine must be fed by
// the one act `space` already means in mic mode. A constant separated from the
// machine that reads it is a constant that drifts. The cross-reference is
// written in both directions instead.
//
// **THE GESTURE IS A SECOND READING OF A KEY THAT IS ALREADY BOUND.** In
// `Mode::Mic`, `r` and `space` already mean `Intent::MicRecord`; no key was
// minted, no table row changed, and the `?` overlay still tells the truth.
// TAP TOGGLES — the behaviour he asked to KEEP — and HOLD STREAMS.

/// **THE BRIDGE ACROSS macOS'S INITIAL REPEAT DELAY.**
///
/// The first auto-repeat does not follow the press at the repeat RATE; it
/// follows it after `InitialKeyRepeat`, which on his machine is UNSET and
/// therefore the system default of about 375 ms. A burst has to survive that
/// one long interval or the first repeat would read as a second deliberate
/// press and toggle the recording straight back off.
///
/// **The cost is named rather than hidden:** a genuine second press of
/// `record` landing inside this window is swallowed. In mic mode that press
/// would have produced a sub-600 ms capture, which this module already reports
/// as *"the recording came back empty"* — so the swallow costs a degenerate
/// capture, not a real one.
pub const HOLD_FIRST_GAP: Duration = Duration::from_millis(600);

/// **THE GAP BETWEEN THE FIRST REPEAT AND THE SECOND, BEFORE THE CADENCE IS
/// KNOWN.** Auto-repeat's steady rate is always far tighter than its initial
/// delay (90 ms against 375 ms at the defaults), so once one interval has been
/// bridged the window tightens to this immediately rather than staying at
/// [`HOLD_FIRST_GAP`] — which is what stops a third deliberate press from being
/// swallowed too.
pub const HOLD_PROVISIONAL_GAP: Duration = Duration::from_millis(200);

/// The floor under a LEARNED gap. `0095` measured his tmux delivering a 90 ms
/// synthetic stream at 51–112 ms inter-arrival — jitter, never a stall — so a
/// gap must clear 112 ms by more than Claude Code's 120 ms does.
pub const HOLD_MIN_GAP: Duration = Duration::from_millis(150);

/// The ceiling over a LEARNED gap, and the point past which a repeat stream is
/// too slow to be one. A machine whose key repeat is slower than half of this
/// never arms the gesture and keeps the tap-toggle unchanged.
pub const HOLD_MAX_GAP: Duration = Duration::from_millis(600);

/// The learned gap is this many times the SLOWEST steady interval observed in
/// the burst in front of us. **This is the number that removes the dependency
/// on a measurement only his keyboard can supply.**
pub const HOLD_GAP_MULT: u32 = 2;

/// Events in one burst before it is a HOLD rather than a tap.
///
/// Three: the press, and then TWO repeats. One repeat is ambiguous with a
/// double-tap; two means a steady cadence has been observed, which is the
/// thing a hand cannot produce. At the macOS defaults that is about 465 ms
/// into the hold — and nothing is waiting on it, because the capture started
/// on the press.
pub const HOLD_ARM: u32 = 3;

/// What the face should do with one `MicRecord` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// **THE FIRST EVENT OF A BURST.** Toggle, exactly as before this gesture
    /// existed. Every tap is one of these and nothing about the tap changed.
    Act,
    /// **THE SECOND EVENT, AND IT IS GENUINELY AMBIGUOUS.** It is either the
    /// first auto-repeat of a hold or a second deliberate press, and ONE
    /// sample cannot tell those apart — the discriminator is whether a THIRD
    /// event follows it at a machine's cadence.
    ///
    /// So it is neither acted on nor thrown away: it is HELD, and the idle gap
    /// decides. A repeat stream discards it; silence applies it, late, through
    /// [`Expire::Deferred`]. **No press this face received is ever silently
    /// eaten**, which is the property that makes the ambiguity affordable —
    /// the cost is that a second press lands up to
    /// [`HOLD_PROVISIONAL_GAP`] after it was made, and the thing it does is
    /// stop a recording, which is not a thing a fifth of a second spoils.
    Defer,
    /// **AUTO-REPEAT.** Dropped. Acting on it would toggle the recording off
    /// a tenth of a second after he started it, and on again after that.
    Repeat,
    /// The repeat that made this burst a HOLD, and the one that discards the
    /// deferred press. Reported apart only so the face can say it is
    /// listening.
    Armed,
}

/// What an expired idle gap means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expire {
    /// The burst was a tap, or there was no burst. Nothing is owed.
    None,
    /// **HE LET GO.** A hold that opened a capture has ended; close it, which
    /// puts the capture on exactly the path a second tap would have.
    EndHold,
    /// **THE HELD PRESS WAS REAL AFTER ALL** — no third event came, so the
    /// second one was his hand and not the keyboard's. Apply it now.
    Deferred,
}

/// **THE HOLD DETECTOR — repeat-counting with an idle gap, learning the
/// machine's own cadence from the burst in front of it.**
///
/// It is fed the `MicRecord` act, not a key, so both spellings of that act
/// (`r` and `space`) carry the gesture and neither this type nor the face has
/// to learn a keycode.
///
/// **NOTHING IS HARD-CODED TO 120 ms.** Claude Code's constant is a starting
/// point and his repeat rate is unmeasured; this learns the interval from the
/// repeats it is already receiving and sets the end-of-hold threshold to
/// [`HOLD_GAP_MULT`] times the slowest one seen. The only numbers it assumes
/// about his keyboard are the two bounds: the initial delay must be under
/// [`HOLD_FIRST_GAP`] and the repeat interval under
/// [`HOLD_PROVISIONAL_GAP`]. At the macOS defaults (375 ms and 90 ms) both
/// clear by more than 3×.
#[derive(Debug, Default)]
pub struct Hold {
    /// When the last event of the open burst arrived. `None` means no burst.
    last: Option<Instant>,
    /// How many events this burst has had, the first one included.
    count: u32,
    /// How many intervals have been observed in this burst.
    intervals: u32,
    /// The SLOWEST steady interval seen — intervals after the first, which is
    /// the initial repeat delay and is not a cadence sample. Slowest rather
    /// than mean because the threshold has to clear the jitter, not describe
    /// it.
    steady: Option<Duration>,
    /// Did this burst's FIRST event start a capture? Only then does its end
    /// have something to stop. A hold that began by stopping a running
    /// recording must not start a new one when the key comes up.
    began_capture: bool,
}

impl Hold {
    /// The idle gap that currently ends the burst — and joins the next event
    /// to it.
    pub fn gap(&self) -> Duration {
        match (self.steady, self.intervals) {
            (Some(s), _) => (s * HOLD_GAP_MULT).clamp(HOLD_MIN_GAP, HOLD_MAX_GAP),
            (None, 0) => HOLD_FIRST_GAP,
            (None, _) => HOLD_PROVISIONAL_GAP,
        }
    }

    /// Whether the open burst has become a hold.
    pub fn armed(&self) -> bool {
        self.count >= HOLD_ARM
    }

    /// **ONE `MicRecord` EVENT.**
    pub fn press(&mut self, now: Instant) -> Press {
        let joins = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) <= self.gap());
        if !joins {
            *self = Hold {
                last: Some(now),
                count: 1,
                ..Hold::default()
            };
            return Press::Act;
        }
        let delta = now.saturating_duration_since(self.last.expect("joins implies a last"));
        self.intervals += 1;
        // **THE FIRST INTERVAL IS THE INITIAL DELAY, NOT THE CADENCE.** Feeding
        // it to the learner would set the end-of-hold threshold to about
        // 750 ms, which is three quarters of a second of silence after he lets
        // go. Every interval after it is a repeat-to-repeat sample.
        if self.intervals >= 2 {
            self.steady = Some(self.steady.map_or(delta, |s| s.max(delta)));
        }
        self.count += 1;
        self.last = Some(now);
        match self.count {
            2 => Press::Defer,
            n if n == HOLD_ARM => Press::Armed,
            _ => Press::Repeat,
        }
    }

    /// Record whether the burst's first event opened the microphone.
    pub fn began_capture(&mut self, yes: bool) {
        self.began_capture = yes;
    }

    /// **WHEN THE FACE OWES ITSELF A WAKE-UP** — `None` when an expiry would
    /// do nothing, so a face that is not mid-hold arms no timer at all.
    pub fn deadline(&self, now: Instant) -> Option<Duration> {
        if self.owed() == Expire::None {
            return None;
        }
        let last = self.last?;
        Some(self.gap().saturating_sub(now.saturating_duration_since(last)))
    }

    /// What this burst would mean if the gap expired now — WITHOUT changing
    /// anything. [`Hold::deadline`] asks it to decide whether a wake-up is
    /// owed at all, and [`Hold::expire`] asks it for the answer.
    fn owed(&self) -> Expire {
        if self.last.is_none() {
            Expire::None
        } else if self.count == 2 {
            Expire::Deferred
        } else if self.armed() && self.began_capture {
            Expire::EndHold
        } else {
            Expire::None
        }
    }

    /// **ANYTHING ELSE HAPPENED, SO THE KEY IS NOT STILL DOWN.**
    ///
    /// A burst is a run of ONE act with nothing between its events. Any other
    /// intent — another key, a click — is proof the hold is over, and it is
    /// proof that arrives sooner than the idle gap would. Without this a burst
    /// would span unrelated work and a `record` press half a second after some
    /// other key would be read as that burst's first repeat.
    ///
    /// It answers with the same [`Expire`] the gap would have, so a press this
    /// burst was holding is applied here rather than lost.
    pub fn interrupt(&mut self) -> Expire {
        if self.last.is_none() {
            return Expire::None;
        }
        let what = self.owed();
        *self = Hold::default();
        what
    }

    /// **THE IDLE GAP EXPIRED.** The burst is closed whatever the answer, so
    /// the next event is a fresh [`Press::Act`].
    pub fn expire(&mut self, now: Instant) -> Expire {
        let Some(last) = self.last else {
            return Expire::None;
        };
        if now.saturating_duration_since(last) < self.gap() {
            return Expire::None;
        }
        let what = self.owed();
        *self = Hold::default();
        what
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16 kHz mono Int16 WAV holding `samples`.
    fn wav(samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        v.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    /// **THE WHOLE TASK, AS ONE ASSERTION.**
    ///
    /// Digital silence must be REFUSED, by name, before the transcriber is
    /// reached — because the transcriber would return `you` at exit 0 and this
    /// face would show him a word he never said.
    #[test]
    fn digital_silence_is_refused_and_never_reaches_the_transcriber() {
        let silent = wav(&[0i16; 1600]);
        assert_eq!(gate(&silent), Err(SttError::DeadAudio { peak: 0 }));
        assert!(gate(&silent)
            .unwrap_err()
            .plainly()
            .contains("heard nothing"));
    }

    /// The gate is a THRESHOLD and not an is-zero test: a stream that is
    /// technically non-zero but carries no signal is just as dead, and the AUI
    /// put the line at 10 for exactly that reason.
    #[test]
    fn a_near_silent_stream_is_dead_and_one_above_the_line_is_not() {
        assert_eq!(
            gate(&wav(&[0, 3, -7, 10, -2])),
            Err(SttError::DeadAudio { peak: 10 })
        );
        assert_eq!(gate(&wav(&[0, 3, -11, 10, -2])), Ok(11));
        // Real speech sails through.
        assert_eq!(gate(&wav(&[0, 9000, -12000, 300])), Ok(12000));
    }

    /// **A FILE WE CANNOT READ IS NOT A DEAD MICROPHONE.** The two messages
    /// accuse different things and the gate must not collapse them.
    #[test]
    fn unreadable_bytes_are_not_reported_as_a_dead_microphone() {
        assert_eq!(gate(b""), Err(SttError::NoAudio));
        assert_eq!(gate(b"not a wav at all"), Err(SttError::NoAudio));
        // A 32-bit float WAV is legible RIFF we decline to judge.
        let mut f32wav = wav(&[0i16; 100]);
        f32wav[34] = 32; // bits_per_sample = 32
        assert_eq!(gate(&f32wav), Err(SttError::NoAudio));
    }

    /// **THE KILLED-WRITER CASE.** A header declaring zero over real speech
    /// must be measured by what is on disk, or a real utterance is called
    /// silent and refused by the gate that exists to protect it.
    #[test]
    fn a_capture_severed_mid_recording_is_measured_by_what_is_actually_there() {
        let mut v = wav(&[0, 9000, -12000, 300]);
        let d = parse_wav(&v).unwrap();
        // Zero the declared data size, as a killed writer leaves it.
        v[d.start - 4..d.start].copy_from_slice(&0u32.to_le_bytes());
        let d2 = parse_wav(&v).unwrap();
        assert_eq!(d2.declared, 0);
        assert!(!d2.declares_the_truth());
        assert_eq!(d2.usable(), 8, "the samples on disk were not counted");
        assert_eq!(gate(&v), Ok(12000), "real speech was called silent");
    }

    /// A header that OVER-declares (truncated file) is also measured by the
    /// disk, so a short read cannot walk off the end.
    #[test]
    fn an_over_declaring_header_cannot_read_past_the_end_of_the_file() {
        let mut v = wav(&[0, 9000, -12000, 300]);
        let d = parse_wav(&v).unwrap();
        v[d.start - 4..d.start].copy_from_slice(&999_999u32.to_le_bytes());
        assert_eq!(gate(&v), Ok(12000));
    }

    /// The command boundary substitutes, and it never goes through a shell.
    #[test]
    fn a_command_substitutes_the_path_and_is_never_word_split_by_a_shell() {
        let c = Cmd::parse("/opt/homebrew/bin/rec -r 16000 {}").unwrap();
        assert_eq!(c.program, "/opt/homebrew/bin/rec");
        let (p, a) = c.resolved(Path::new("/tmp/a b.wav"));
        assert_eq!(p, "/opt/homebrew/bin/rec");
        // The space in the path stays INSIDE one argument.
        assert_eq!(
            a,
            vec!["-r".to_string(), "16000".into(), "/tmp/a b.wav".into()]
        );
        assert!(Cmd::parse("   ").is_none());
    }

    /// The default capture spec is the one the scout validated, and it is a
    /// STABLE path — the whole permission argument rests on that.
    #[test]
    fn the_default_capture_command_is_the_stable_path_helper_at_the_proven_spec() {
        let c = Cmd::parse(CAPTURE_DEFAULT).unwrap();
        assert_eq!(c.program, "/opt/homebrew/bin/rec");
        let joined = c.args.join(" ");
        assert!(joined.contains("-r 16000"), "{joined}");
        assert!(joined.contains("-c 1"), "{joined}");
        assert!(joined.contains("-b 16"), "{joined}");
        assert!(joined.contains("-e signed-integer"), "{joined}");
    }

    /// **THE CONTENTION REFUSAL FIRES ON A FRESH CAPTURE AND NOT ON AN OLD
    /// ONE**, so it refuses while he is talking to the AUI and does not refuse
    /// forever afterwards.
    #[test]
    fn the_aui_guard_sees_a_live_capture_and_ignores_a_finished_one() {
        let d = std::env::temp_dir().join(format!("tui-mic-guard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let now = SystemTime::now();
        assert!(
            !aui_is_capturing_in(&d, now),
            "an empty store is not a live capture"
        );

        fs::write(d.join("cap-1.wav"), b"x").unwrap();
        assert!(
            aui_is_capturing_in(&d, now),
            "a just-written capture was not seen"
        );

        // The same file, judged from far enough in the future, is history.
        let later = now + Duration::from_secs(60);
        assert!(
            !aui_is_capturing_in(&d, later),
            "a stale capture refused forever"
        );

        // A non-wav is not a capture.
        fs::remove_file(d.join("cap-1.wav")).unwrap();
        fs::write(d.join("notes.txt"), b"x").unwrap();
        assert!(!aui_is_capturing_in(&d, now));
        let _ = fs::remove_dir_all(&d);
    }

    /// **0600 IN 0700, CHECKED ON DISK**, exactly as the draft journal's
    /// exception is checked. His voice is at least as private as his draft.
    #[test]
    fn a_capture_is_created_private_in_a_private_directory() {
        let p = new_capture_path().expect("a capture path");
        let m = fs::metadata(&p).unwrap();
        assert_eq!(
            m.permissions().mode() & 0o777,
            FILE_MODE,
            "the capture is not 0600"
        );
        let dm = fs::metadata(dir().unwrap()).unwrap();
        assert_eq!(
            dm.permissions().mode() & 0o777,
            DIR_MODE,
            "the capture dir is not 0700"
        );
        let _ = fs::remove_file(&p);
    }

    /// The header repair puts a severed capture back together on disk, and
    /// leaves an honest one alone.
    #[test]
    fn the_header_repair_rewrites_only_a_header_that_lies() {
        let d = std::env::temp_dir().join(format!("tui-mic-repair-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();

        let good = d.join("good.wav");
        fs::write(&good, wav(&[0, 9000, -12000, 300])).unwrap();
        assert!(
            !repair_header(&good).unwrap(),
            "an honest header was rewritten"
        );

        let bad = d.join("bad.wav");
        let mut v = wav(&[0, 9000, -12000, 300]);
        let ds = parse_wav(&v).unwrap().start;
        v[ds - 4..ds].copy_from_slice(&0u32.to_le_bytes());
        v[4..8].copy_from_slice(&0u32.to_le_bytes());
        fs::write(&bad, &v).unwrap();
        assert!(
            repair_header(&bad).unwrap(),
            "a zero-declaring header was not repaired"
        );

        let back = fs::read(&bad).unwrap();
        let d2 = parse_wav(&back).unwrap();
        assert!(
            d2.declares_the_truth(),
            "the repair did not make the header honest"
        );
        assert_eq!(d2.declared, 8);
        assert_eq!(u32_le(&back, 4).unwrap() as usize, back.len() - 8);
        let _ = fs::remove_dir_all(&d);
    }

    // ─────────────────────────────────────────────────────────────────────
    //  THE HOLD DETECTOR — THE REPEAT TABLE
    // ─────────────────────────────────────────────────────────────────────

    /// Drive a burst at a fixed cadence and say what the face saw.
    ///
    /// **THE CLOCK IS INJECTED, NOT SLEPT.** A timing machine proven with real
    /// sleeps is a machine whose test is a load measurement; every interval
    /// below is exact and the table means the same thing on a busy laptop.
    /// `0046`: *if a timing-sensitive test goes red, suspect machine load* —
    /// this one cannot be moved by it.
    fn burst(first: Duration, rest: Duration, n: u32) -> (Hold, Vec<Press>, Instant) {
        let mut h = Hold::default();
        let t0 = Instant::now();
        let mut out = vec![h.press(t0)];
        // The gesture's FIRST event is the one that opens the microphone, and
        // the detector has to be told so or the end of the hold has nothing to
        // stop.
        h.began_capture(true);
        let mut t = t0;
        for i in 1..n {
            t += if i == 1 { first } else { rest };
            out.push(h.press(t));
        }
        (h, out, t)
    }

    /// **THE REPEAT TABLE — the whole viability question, one row per cadence.**
    ///
    /// A stream FASTER than the threshold must read as a hold; one SLOWER must
    /// not, and must stay the tap-toggle he already has. The macOS default row
    /// is the one his machine is on: `KeyRepeat` and `InitialKeyRepeat` are
    /// both unset, which is the system default of about 375 ms then 90 ms.
    #[test]
    fn the_repeat_table_reads_a_hold_only_where_a_keyboard_could_have_produced_one() {
        // (label, initial delay, steady interval, is it a hold)
        let table: &[(&str, u64, u64, bool)] = &[
            ("macOS defaults: 375 ms then 90 ms", 375, 90, true),
            ("the fastest settings: 250 ms then 15 ms", 250, 15, true),
            ("his tmux's measured worst jitter: 112 ms", 375, 112, true),
            ("a slow repeat rate, 500 ms apart", 375, 500, false),
            ("deliberate taps, 700 ms apart", 700, 700, false),
            ("deliberate taps, 250 ms apart", 250, 250, false),
        ];
        for (label, first, rest, is_hold) in table {
            let (h, seen, _) = burst(
                Duration::from_millis(*first),
                Duration::from_millis(*rest),
                8,
            );
            assert_eq!(
                h.armed(),
                *is_hold,
                "{label}: armed={} but the table says {is_hold}",
                h.armed()
            );
            assert_eq!(seen[0], Press::Act, "{label}: the first event must ACT");
            if *is_hold {
                assert!(
                    seen[1..].iter().all(|p| *p != Press::Act),
                    "{label}: a repeat toggled the recording"
                );
                assert_eq!(
                    seen.iter().filter(|p| **p == Press::Defer).count(),
                    1,
                    "{label}: the ambiguous second event was not held exactly once"
                );
                assert_eq!(
                    seen.iter().filter(|p| **p == Press::Armed).count(),
                    1,
                    "{label}: the face was told it was listening more than once"
                );
            }
        }
    }

    /// **A TAP IS STILL A TOGGLE AND ITS BURST ENDS QUIETLY.** The gesture he
    /// asked to KEEP: one press, no repeat, nothing auto-stops it.
    #[test]
    fn one_press_never_becomes_a_hold_and_never_stops_itself() {
        let mut h = Hold::default();
        let t0 = Instant::now();
        assert_eq!(h.press(t0), Press::Act);
        h.began_capture(true);
        assert!(!h.armed(), "one press armed a hold");
        assert!(
            h.deadline(t0).is_none(),
            "a tap armed a timer that would have stopped his recording under him"
        );
        assert_eq!(
            h.expire(t0 + Duration::from_secs(5)),
            Expire::None,
            "a tap's recording was stopped by the idle gap five seconds later"
        );
    }

    /// **THE IDLE GAP ENDS THE HOLD, AND ONLY AFTER IT HAS ELAPSED.**
    #[test]
    fn the_hold_ends_on_the_idle_gap_and_not_before_it() {
        let (mut h, _, last) = burst(
            Duration::from_millis(375),
            Duration::from_millis(90),
            8,
        );
        assert!(h.armed());
        let gap = h.gap();
        assert_eq!(
            h.expire(last + gap - Duration::from_millis(1)),
            Expire::None,
            "the hold ended one millisecond early — a stutter would split an utterance"
        );
        let d = h
            .deadline(last)
            .expect("a hold in flight owes the face a wake-up");
        assert_eq!(d, gap, "the wake-up is not the gap it is waiting for");
        assert_eq!(
            h.expire(last + gap),
            Expire::EndHold,
            "the hold never ended — he let go and the microphone stayed open"
        );
        assert!(!h.armed(), "the burst outlived its own expiry");
        assert_eq!(
            h.press(last + gap + Duration::from_secs(1)),
            Press::Act,
            "the press after a hold was eaten as if the hold were still running"
        );
    }

    /// **THE GAP IS LEARNED FROM THE BURST IN FRONT OF IT**, which is what
    /// removes the dependency on a repeat rate only his keyboard can supply.
    /// A fast repeater gets a tight gap; a slow one gets a loose one; neither
    /// number is written down anywhere.
    #[test]
    fn the_gap_is_learned_from_the_cadence_and_not_copied_from_a_constant() {
        let (fast, _, _) = burst(Duration::from_millis(375), Duration::from_millis(15), 8);
        let (slow, _, _) = burst(Duration::from_millis(375), Duration::from_millis(180), 8);
        assert!(
            fast.gap() < slow.gap(),
            "two machines three orders of magnitude apart got the same threshold: \
             {:?} vs {:?}",
            fast.gap(),
            slow.gap()
        );
        assert_eq!(fast.gap(), HOLD_MIN_GAP, "the floor is not holding");
        assert_eq!(slow.gap(), Duration::from_millis(360));
        // The INITIAL DELAY must never reach the learner: 2 x 375 ms would put
        // three quarters of a second of silence after every release.
        assert!(
            slow.gap() < Duration::from_millis(750),
            "the initial repeat delay was learned as if it were the cadence"
        );
    }

    /// **A HOLD THAT BEGAN BY STOPPING A RECORDING HAS NOTHING TO STOP.**
    /// Holding the key while a capture is already running must not start a new
    /// one when the key comes up.
    #[test]
    fn a_hold_that_opened_nothing_does_not_close_anything() {
        let mut h = Hold::default();
        let t0 = Instant::now();
        assert_eq!(h.press(t0), Press::Act);
        h.began_capture(false); // the press STOPPED a running capture
        let mut t = t0 + Duration::from_millis(375);
        h.press(t);
        for _ in 0..6 {
            t += Duration::from_millis(90);
            h.press(t);
        }
        assert!(h.armed(), "the burst was not read as a hold at all");
        assert!(
            h.deadline(t).is_none(),
            "a hold with no capture behind it armed a timer"
        );
        assert_eq!(
            h.expire(t + Duration::from_secs(1)),
            Expire::None,
            "the end of the hold would have started a recording he did not ask for"
        );
    }

    /// **THE MODEL IS `large-v3-turbo` BY DEFAULT ON BOTH PATHS** (`IS-53`),
    /// and an explicit `TUI_MIC_MODEL` still wins.
    #[test]
    fn the_model_default_is_large_and_the_override_still_wins() {
        // Serialised against nothing: this test owns the variable for its own
        // duration and restores it, and no other test in this binary reads it.
        let before = std::env::var(MODEL_VAR).ok();
        std::env::remove_var(MODEL_VAR);
        // **NOT `is_none()` OR AN EARLY RETURN — the absence trap in its
        // purest form.** "No model was resolved" is scored triumphantly by a
        // ladder that lost a rung. The candidates are named here, so this
        // asserts a POSITIVE fact whenever the machine has one of them, and
        // deleting a rung goes red rather than quiet. A machine with neither
        // is a legal state `transcribe` logs; it is the only case that
        // asserts nothing, and it says so.
        let home = dirs::home_dir().expect("a home directory");
        let candidates = [
            home.join(".aui").join("models").join(LARGE_MODEL_FILE),
            home.join("code")
                .join("whisper.cpp")
                .join("models")
                .join(LARGE_MODEL_FILE),
        ];
        match candidates.iter().find(|c| c.exists()) {
            Some(want) => assert_eq!(
                model_path().as_ref(),
                Some(want),
                "the large model is at {} and the ladder did not reach it",
                want.display()
            ),
            None => eprintln!(
                "CANNOT-CHECK: no {LARGE_MODEL_FILE} on this machine, so the \
                 default cannot be asserted. This is not a pass."
            ),
        }
        std::env::set_var(MODEL_VAR, "/tmp/some-other-model.bin");
        assert_eq!(
            model_path(),
            Some(PathBuf::from("/tmp/some-other-model.bin")),
            "an explicit override was second-guessed"
        );
        match before {
            Some(v) => std::env::set_var(MODEL_VAR, v),
            None => std::env::remove_var(MODEL_VAR),
        }
    }

    /// **A DOUBLE-TAP TOO FAST TO TELL FROM A REPEAT IS NOT EATEN — IT IS
    /// LATE.**
    ///
    /// This is the one honest cost of having no key-release event, and the
    /// reason it is affordable is that the press is DEFERRED rather than
    /// dropped: no keystroke this face received disappears. A stream that
    /// continues discards it; silence applies it.
    #[test]
    fn a_second_press_that_no_repeat_follows_is_applied_late_and_never_lost() {
        let mut h = Hold::default();
        let t0 = Instant::now();
        assert_eq!(h.press(t0), Press::Act);
        h.began_capture(true);

        // 250 ms later — well inside the window that bridges macOS's initial
        // repeat delay, so it CANNOT be told from the first auto-repeat yet.
        let t1 = t0 + Duration::from_millis(250);
        assert_eq!(h.press(t1), Press::Defer);
        assert!(
            h.deadline(t1).is_some(),
            "the held press armed no wake-up — it would have been lost forever"
        );
        assert_eq!(
            h.expire(t1 + HOLD_PROVISIONAL_GAP),
            Expire::Deferred,
            "his second press vanished"
        );
        assert_eq!(
            h.press(t1 + Duration::from_secs(2)),
            Press::Act,
            "the burst outlived the press it deferred"
        );

        // …and the SAME second event, when a repeat stream follows it, is a
        // repeat and is discarded. One event, two readings, decided by what
        // comes after it.
        let mut h = Hold::default();
        let t0 = Instant::now();
        h.press(t0);
        h.began_capture(true);
        let t1 = t0 + Duration::from_millis(250);
        assert_eq!(h.press(t1), Press::Defer);
        let t2 = t1 + Duration::from_millis(90);
        assert_eq!(h.press(t2), Press::Armed);
        assert_eq!(
            h.expire(t2 + h.gap()),
            Expire::EndHold,
            "the deferred press survived into a hold and would have toggled twice"
        );
    }
}
