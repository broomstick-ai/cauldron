//! # THE RUNNING IMAGE — what binary is this process actually executing?
//!
//! This module answers one question, and it answers it about the **image the
//! process is executing**, never about a path.
//!
//! ## Why it exists
//!
//! Three separate investigations got this wrong inside one week:
//!
//! 1. One concluded five days of committed work was live when it was not.
//! 2. One concluded four days of commits had *never* run when they had — it
//!    read the process's elapsed time as the daemon's age, not knowing that
//!    [`crate::update`] re-execs **in place and keeps its pid**. `ps` shows the
//!    original launch; the image can be four re-execs newer.
//! 3. `daemon.status` reported a `binary` field holding a **path**, and the
//!    file at that path had since been replaced by somebody's `cargo build`.
//!    Anyone who inspected it examined a different, unsigned image — not what
//!    was running. That field is now gone; see [`launch_path_holds_running_image`].
//!
//! ## The governing rule
//!
//! **Every field describes the image the process is executing.** Where a fact
//! can only be derived from a path, it lives under `launch_path` (which is
//! explicitly *the file at that path right now*) or it is marked with the
//! source it was attributed from. A path can be replaced underneath a running
//! process; an inode cannot.
//!
//! ## The second rule: an unanswerable question must not return a confident value
//!
//! Every field that can fail is a [`Fact`] — `{value, known, why}`. When it
//! cannot be determined, `known` is `false`, `value` is `null`, and `why` says
//! what was tried and what happened. There is no field in this payload where a
//! `null` could be mistaken for "fine". This whole class of bug is instruments
//! reporting "fine" when they mean "I could not see"; this module does not add
//! another one.
//!
//! ## Where the facts come from
//!
//! | fact | source | describes |
//! |---|---|---|
//! | running cdhash | `csops(CS_OPS_CDHASH)` | the running image, from the kernel |
//! | running identifier | `csops(CS_OPS_IDENTITY)` | the running image, from the kernel |
//! | running ad-hoc / CMS | `csops(CS_OPS_BLOB)`, parsed | the running image, from the kernel |
//! | text inode | `proc_pidinfo(PROC_PIDREGIONPATHINFO)` | the running image, from the kernel |
//! | authority, designated requirement | `codesign` on an **attributed** file | the running image, *if* attribution succeeded |
//! | build time, commit | compiled into this image by `build.rs` | the running image |
//! | launch-path signature | `codesign` on the path | **the file at that path now** |
//!
//! The kernel calls are the load-bearing ones. `csops(2)` and `proc_pidinfo(2)`
//! ask the kernel about *this pid*; they cannot be fooled by replacing a file,
//! which is precisely the failure that produced this module.
//!
//! ## Attribution — how a path-derived fact is made honest
//!
//! Two facts the operator asked for (the **authority** and the **designated
//! requirement**) live in the CMS blob and cannot be had from the kernel
//! without implementing certificate parsing. So they are read off a file — but
//! only off a file whose **cdhash equals the kernel's cdhash for this
//! process**. Code signing is content-addressed: an identical cdhash means an
//! identical code directory, so that file's signature *is* the running image's
//! signature. The candidates are tried in order:
//!
//! 1. the launch path — usually the running image, but not after a rebuild;
//! 2. `<data-dir>/threads.last-good`, which [`crate::update::UpdateGate::keep_last_good`]
//!    writes **from the running image at boot**, so it survives exactly the
//!    replacement that breaks candidate 1.
//!
//! If neither matches, those fields are undeterminable and say so. They are
//! never guessed from the launch path, because guessing from the launch path is
//! the original bug.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::update::STABLE_BUNDLE_ID;

// ──────────────────────────────────────────────────────────────────────────
//  Fact — a value that might not be knowable
// ──────────────────────────────────────────────────────────────────────────

/// A value that is either **known** or explicitly **undeterminable**.
///
/// Serialises to `{"value": …, "known": true}` or
/// `{"value": null, "known": false, "why": "<what was tried, and what happened>"}`.
///
/// The shape is deliberately not a bare `null`. A reader scanning this payload
/// cannot accidentally read a missing measurement as a measured absence, which
/// is the exact confusion that made three investigations wrong.
#[derive(Debug, Clone)]
pub struct Fact {
    value: Option<Value>,
    why: Option<String>,
}

impl Fact {
    /// A determined value.
    pub fn known(v: impl Into<Value>) -> Self {
        Self { value: Some(v.into()), why: None }
    }

    /// Undeterminable, with the reason a human needs.
    pub fn unknown(why: impl Into<String>) -> Self {
        Self { value: None, why: Some(why.into()) }
    }

    /// `Some` → known; `None` → undeterminable for `why`.
    pub fn of<T: Into<Value>>(v: Option<T>, why: impl Into<String>) -> Self {
        match v {
            Some(v) => Self::known(v),
            None => Self::unknown(why),
        }
    }

    /// Is this fact determined?
    pub fn is_known(&self) -> bool {
        self.value.is_some()
    }

    /// The determined value, if any.
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    fn as_str(&self) -> Option<&str> {
        self.value.as_ref().and_then(Value::as_str)
    }

    /// The wire form.
    pub fn json(&self) -> Value {
        match (&self.value, &self.why) {
            (Some(v), _) => json!({ "value": v, "known": true }),
            (None, why) => json!({
                "value": Value::Null,
                "known": false,
                "why": why.clone().unwrap_or_else(|| "undeterminable".into()),
            }),
        }
    }
}

impl From<Fact> for Value {
    fn from(f: Fact) -> Value {
        f.json()
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The kernel — csops(2)
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod kernel {
    use std::io::Error as IoError;

    // `csops` is exported by libSystem. It is how launchd, `codesign --verify
    // --pid` and the App Store daemon ask the kernel what code identity a
    // RUNNING process has. Asking it about ourselves is always permitted.
    extern "C" {
        fn csops(
            pid: libc::pid_t,
            ops: libc::c_uint,
            useraddr: *mut libc::c_void,
            usersize: libc::size_t,
        ) -> libc::c_int;
    }

    pub const CS_OPS_STATUS: libc::c_uint = 0;
    pub const CS_OPS_CDHASH: libc::c_uint = 5;
    pub const CS_OPS_BLOB: libc::c_uint = 10;
    pub const CS_OPS_IDENTITY: libc::c_uint = 11;
    pub const CS_OPS_TEAMID: libc::c_uint = 14;

    /// The code directory hash of the image `pid` is executing — 20 bytes,
    /// straight from the kernel's record for the process.
    ///
    /// This is THE authoritative answer to "what is running". It does not read
    /// any file, so replacing the file at the launch path cannot change it.
    pub fn cdhash(pid: libc::pid_t) -> Result<String, String> {
        let mut h = [0u8; 20];
        let rc = unsafe {
            csops(pid, CS_OPS_CDHASH, h.as_mut_ptr() as *mut libc::c_void, h.len())
        };
        if rc != 0 {
            return Err(format!(
                "csops(CS_OPS_CDHASH) on pid {pid} failed: {}",
                IoError::last_os_error()
            ));
        }
        Ok(h.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// The kernel's code-signing status flags for `pid`.
    ///
    /// **Deliberately NOT used to decide ad-hoc-ness.** Measured on macOS 25.6
    /// while writing this: an ad-hoc process and a properly signed one both
    /// report `CS_ADHOC` clear (`0x22000201` for each), so this flag word
    /// cannot tell them apart. The CodeDirectory in [`blob`] can, and does.
    /// The flags are reported for completeness and are labelled as such.
    pub fn status(pid: libc::pid_t) -> Result<u32, String> {
        let mut st: u32 = 0;
        let rc = unsafe {
            csops(
                pid,
                CS_OPS_STATUS,
                &mut st as *mut u32 as *mut libc::c_void,
                std::mem::size_of::<u32>(),
            )
        };
        if rc != 0 {
            return Err(format!(
                "csops(CS_OPS_STATUS) on pid {pid} failed: {}",
                IoError::last_os_error()
            ));
        }
        Ok(st)
    }

    /// A NUL-terminated string op (`CS_OPS_IDENTITY`, `CS_OPS_TEAMID`).
    ///
    /// The kernel copies out an 8-byte header and then the string, so the
    /// payload starts at offset 8.
    fn string_op(pid: libc::pid_t, op: libc::c_uint, what: &str) -> Result<String, String> {
        let mut buf = vec![0u8; 1024];
        let rc = unsafe {
            csops(pid, op, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        if rc != 0 {
            return Err(format!("csops({what}) on pid {pid} failed: {}", IoError::last_os_error()));
        }
        let body = &buf[8..];
        let end = body.iter().position(|b| *b == 0).unwrap_or(0);
        if end == 0 {
            return Err(format!("csops({what}) on pid {pid} returned an empty string"));
        }
        String::from_utf8(body[..end].to_vec())
            .map_err(|e| format!("csops({what}) on pid {pid} returned invalid UTF-8: {e}"))
    }

    /// The signing IDENTIFIER of the running image (e.g. `com.harness.threads`).
    pub fn identity(pid: libc::pid_t) -> Result<String, String> {
        string_op(pid, CS_OPS_IDENTITY, "CS_OPS_IDENTITY")
    }

    /// The team identifier of the running image. Absent on a self-signed
    /// certificate, which is what `harness-codesign` is — so "not set" here is
    /// normal and is NOT a defect.
    pub fn team_id(pid: libc::pid_t) -> Result<String, String> {
        string_op(pid, CS_OPS_TEAMID, "CS_OPS_TEAMID")
    }

    /// The running image's embedded signature superblob, copied out of the
    /// kernel.
    pub fn blob(pid: libc::pid_t) -> Result<Vec<u8>, String> {
        // Signature blobs for this daemon measure ~60 KB (a 58 KB code
        // directory over a 30 MB debug binary, plus a 1.7 KB CMS blob). 4 MiB
        // is a ceiling nothing realistic reaches; ERANGE below says so plainly
        // rather than silently truncating.
        let mut buf = vec![0u8; 4 << 20];
        let rc = unsafe {
            csops(pid, CS_OPS_BLOB, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        if rc != 0 {
            return Err(format!(
                "csops(CS_OPS_BLOB) on pid {pid} failed: {}",
                IoError::last_os_error()
            ));
        }
        Ok(buf)
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The kernel — the TEXT VNODE, which is the thing a path cannot lie about
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod text_vnode {
    use std::io::Error as IoError;

    const PROC_PIDREGIONPATHINFO: libc::c_int = 8;

    /// `struct proc_regioninfo` from `<sys/proc_info.h>`. 96 bytes; asserted
    /// below.
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct ProcRegionInfo {
        pri_protection: u32,
        pri_max_protection: u32,
        pri_inheritance: u32,
        pri_flags: u32,
        pri_offset: u64,
        pri_behavior: u32,
        pri_user_tag: u32,
        pri_pages_resident: u32,
        pri_pages_shared_now_private: u32,
        pri_pages_swapped_out: u32,
        pri_pages_dirtied: u32,
        pri_ref_count: u32,
        pri_shadow_depth: u32,
        pri_share_mode: u32,
        pri_private_pages_resident: u32,
        pri_shared_pages_resident: u32,
        pri_obj_id: u32,
        pri_depth: u32,
        pri_address: u64,
        pri_size: u64,
    }

    /// `struct proc_regionwithpathinfo`. The `vnode_info_path` half comes from
    /// `libc`, so only the region half is hand-declared.
    #[repr(C)]
    struct ProcRegionWithPathInfo {
        prp_prinfo: ProcRegionInfo,
        prp_vip: libc::vnode_info_path,
    }

    // The layouts were verified against the system headers on macOS 25.6 by
    // compiling `<sys/proc_info.h>` and printing `sizeof`/`offsetof`:
    //
    //     sizeof proc_regioninfo = 96
    //     sizeof vnode_info_path = 1176
    //     sizeof proc_regionwithpathinfo = 1272   offsetof prp_vip = 96
    //
    // If a future SDK moves any of these, THIS BREAKS THE BUILD rather than
    // silently reading a wrong integer and reporting it as an inode. A wrong
    // inode reported confidently is the failure mode this whole module exists
    // to prevent, so it must never be a runtime surprise.
    const _: () = assert!(std::mem::size_of::<ProcRegionInfo>() == 96);
    const _: () = assert!(std::mem::size_of::<libc::vnode_info_path>() == 1176);
    const _: () = assert!(std::mem::size_of::<ProcRegionWithPathInfo>() == 1272);

    /// What the kernel says backs the executing text pages.
    pub struct TextVnode {
        /// The inode of the file the process is EXECUTING. Immune to the file
        /// at the launch path being replaced — that is the entire point.
        pub inode: u64,
        pub device: u32,
        /// The name the kernel still holds for that vnode. **Not evidence**:
        /// an unlinked vnode keeps its old name, so this string is identical
        /// whether or not the file was replaced. Reported for orientation only.
        pub kernel_path: String,
    }

    /// An anchor guaranteed to live in this image's `__TEXT` segment.
    ///
    /// Its address is what we ask the kernel about, so the region we get back
    /// is unambiguously our own executable — never a dylib, and never
    /// whichever mapping happens to sort first.
    #[inline(never)]
    extern "C" fn text_anchor() {
        std::hint::black_box(());
    }

    /// The vnode backing the running image's text, for our own process.
    pub fn of_self() -> Result<TextVnode, String> {
        let addr = text_anchor as *const () as u64;
        let mut rwpi: ProcRegionWithPathInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ProcRegionWithPathInfo>();
        let rc = unsafe {
            libc::proc_pidinfo(
                std::process::id() as libc::c_int,
                PROC_PIDREGIONPATHINFO,
                addr,
                &mut rwpi as *mut ProcRegionWithPathInfo as *mut libc::c_void,
                size as libc::c_int,
            )
        };
        if rc <= 0 {
            return Err(format!(
                "proc_pidinfo(PROC_PIDREGIONPATHINFO) failed: {}",
                IoError::last_os_error()
            ));
        }
        if rc as usize != size {
            return Err(format!(
                "proc_pidinfo(PROC_PIDREGIONPATHINFO) returned {rc} bytes, expected {size} — \
                 the kernel's struct layout is not the one this build was compiled against, so \
                 the inode it returned is NOT trustworthy and is being discarded"
            ));
        }
        let vi = &rwpi.prp_vip.vip_vi;
        let inode = vi.vi_stat.vst_ino;
        if inode == 0 {
            return Err("the kernel reported inode 0 for the executing text region".into());
        }
        // `libc` models `vip_path` as `[[c_char; 32]; 32]` rather than
        // `[c_char; MAXPATHLEN]` for old-rustc reasons, so flatten it.
        let bytes: Vec<u8> = rwpi
            .prp_vip
            .vip_path
            .iter()
            .flatten()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        Ok(TextVnode {
            inode,
            device: vi.vi_stat.vst_dev,
            kernel_path: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The signature superblob
// ──────────────────────────────────────────────────────────────────────────

const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xfade_0c02;
const CSSLOT_CODEDIRECTORY: u32 = 0;
const CSSLOT_SIGNATURESLOT: u32 = 0x1_0000;
/// `CS_ADHOC` in the CodeDirectory flags word.
const CD_FLAG_ADHOC: u32 = 0x0000_0002;
/// `CS_LINKER_SIGNED` — what the linker leaves on a plain `cargo build`.
const CD_FLAG_LINKER_SIGNED: u32 = 0x0002_0000;

/// What a parsed superblob tells us about the image it came from.
#[derive(Debug, Clone)]
pub struct SignatureFacts {
    /// The signing identifier out of the CodeDirectory.
    pub identifier: Option<String>,
    /// The CodeDirectory flags word.
    pub cd_flags: u32,
    /// `CS_ADHOC` is set.
    pub ad_hoc: bool,
    /// `CS_LINKER_SIGNED` is set — the linker signed it, i.e. nobody did.
    pub linker_signed: bool,
    /// Length of the CMS signature slot. An ad-hoc signature carries the
    /// 8-byte header and nothing else, so `> 8` means a real CMS blob is
    /// present and somebody with a private key produced it.
    pub cms_len: u32,
}

impl SignatureFacts {
    /// Is this a real signature by a real key, as opposed to ad-hoc?
    ///
    /// Two independent signals, and they must agree. `CS_ADHOC` is the
    /// signer's own declaration; the CMS length is the physical presence of a
    /// signature. Requiring both means a hand-crafted CodeDirectory that
    /// simply clears the ad-hoc bit does not read as signed.
    pub fn properly_signed(&self) -> bool {
        !self.ad_hoc && !self.linker_signed && self.cms_len > 8
    }
}

fn be32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Parse an embedded-signature superblob.
///
/// Every read is bounds-checked and every failure is an `Err` with a reason,
/// because "I could not parse the signature" must never render as "unsigned".
/// Those are different answers and only one of them is a deploy problem.
pub fn parse_superblob(buf: &[u8]) -> Result<SignatureFacts, String> {
    let magic = be32(buf, 0).ok_or("the signature blob is shorter than its own header")?;
    if magic != CSMAGIC_EMBEDDED_SIGNATURE {
        return Err(format!(
            "the blob's magic is {magic:#010x}, not an embedded signature superblob \
             ({CSMAGIC_EMBEDDED_SIGNATURE:#010x})"
        ));
    }
    let count = be32(buf, 8).ok_or("the superblob has no index count")?;
    if count > 64 {
        return Err(format!("the superblob claims {count} slots, which is not credible"));
    }

    let mut identifier = None;
    let mut cd_flags = None;
    let mut cms_len = 0u32;

    for i in 0..count as usize {
        let ty = be32(buf, 12 + i * 8).ok_or("the superblob index is truncated")?;
        let off = be32(buf, 16 + i * 8).ok_or("the superblob index is truncated")? as usize;
        let blen = be32(buf, off + 4).unwrap_or(0);
        match ty {
            CSSLOT_CODEDIRECTORY => {
                let bmagic = be32(buf, off).ok_or("the code directory slot is truncated")?;
                if bmagic != CSMAGIC_CODEDIRECTORY {
                    return Err(format!(
                        "the code directory slot holds magic {bmagic:#010x}, not \
                         {CSMAGIC_CODEDIRECTORY:#010x}"
                    ));
                }
                cd_flags = Some(be32(buf, off + 12).ok_or("the code directory has no flags")?);
                let ident_off =
                    be32(buf, off + 20).ok_or("the code directory has no identifier offset")?
                        as usize;
                let start = off + ident_off;
                if let Some(rest) = buf.get(start..) {
                    let end = rest.iter().position(|b| *b == 0).unwrap_or(0);
                    if end > 0 {
                        identifier = String::from_utf8(rest[..end].to_vec()).ok();
                    }
                }
            }
            CSSLOT_SIGNATURESLOT => cms_len = blen,
            _ => {}
        }
    }

    let cd_flags = cd_flags.ok_or("the superblob carries no code directory")?;
    Ok(SignatureFacts {
        identifier,
        cd_flags,
        ad_hoc: cd_flags & CD_FLAG_ADHOC != 0,
        linker_signed: cd_flags & CD_FLAG_LINKER_SIGNED != 0,
        cms_len,
    })
}

// ──────────────────────────────────────────────────────────────────────────
//  Path-derived signature facts
// ──────────────────────────────────────────────────────────────────────────

/// Everything `codesign` will tell us about **the file at a path right now**.
///
/// Every field here is path-derived by construction. Nothing in this struct may
/// be attributed to the running image without a cdhash match first — see
/// [`attribute`].
#[derive(Debug, Clone, Default)]
pub struct OnDisk {
    /// Is there a file at that path at all?
    ///
    /// Load-bearing. `is_stably_signed` returns `false` for a file that does
    /// not exist, and reporting that `false` as "not signed for deploy" would
    /// be a confident answer to an unanswerable question — the exact defect
    /// this module exists to remove. Every signing field below is meaningless
    /// unless this is `true`.
    pub exists: bool,
    pub cdhash: Option<String>,
    pub identifier: Option<String>,
    pub authority: Option<String>,
    pub designated_requirement: Option<String>,
    pub ad_hoc: Option<bool>,
    signed_for_deploy: bool,
}

impl OnDisk {
    /// Is the file at that path signed for deploy? `None` when there is no
    /// file to ask about.
    pub fn signed_for_deploy(&self) -> Option<bool> {
        self.exists.then_some(self.signed_for_deploy)
    }
}

/// Read `codesign -dvvv` and `codesign -d -r-` for `path`.
pub fn on_disk(path: &Path) -> OnDisk {
    let mut out = OnDisk::default();
    out.exists = path.exists();
    if !out.exists {
        return out;
    }
    if let Ok(o) = std::process::Command::new("/usr/bin/codesign")
        .arg("-dvvv")
        .arg(path)
        .output()
    {
        // codesign writes its info output to stderr.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stderr),
            String::from_utf8_lossy(&o.stdout)
        );
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("CDHash=") {
                out.cdhash = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Identifier=") {
                out.identifier = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Authority=") {
                // The FIRST Authority line is the leaf — the signer. Later
                // ones are the chain up to the root.
                if out.authority.is_none() {
                    out.authority = Some(v.trim().to_string());
                }
            } else if line.starts_with("CodeDirectory ") {
                // `... flags=0x2(adhoc) ...` — the same bit the kernel reports
                // for the running image, read here off the file.
                if let Some(f) = line.split_whitespace().find_map(|t| t.strip_prefix("flags=")) {
                    let hex = f.split('(').next().unwrap_or("").trim_start_matches("0x");
                    if let Ok(v) = u32::from_str_radix(hex, 16) {
                        out.ad_hoc = Some(v & CD_FLAG_ADHOC != 0);
                    }
                }
            }
        }
    }
    if let Ok(o) = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r", "-"])
        .arg(path)
        .output()
    {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        for line in text.lines() {
            if let Some(v) = line.trim().strip_prefix("designated =>") {
                out.designated_requirement = Some(v.trim().to_string());
            } else if let Some(v) = line.trim().strip_prefix("# designated =>") {
                out.designated_requirement = Some(v.trim().to_string());
            }
        }
    }
    // The ONE question the absorb path actually asks. Delegated to
    // `update::is_stably_signed` rather than re-derived here, so there is
    // exactly one definition of "signed for deploy" in the crate and this
    // endpoint cannot drift away from the gate it is reporting on.
    out.signed_for_deploy = crate::update::is_stably_signed(path);
    out
}

/// A path-derived signature set, plus the proof it may be attributed to the
/// running image.
struct Attributed {
    facts: OnDisk,
    source: PathBuf,
}

/// Find a file whose cdhash equals the running image's, so its path-derived
/// signature facts genuinely describe the running image.
///
/// Returns the first match, or the list of what was tried and why each failed.
fn attribute(running_cdhash: &str, candidates: &[(&str, PathBuf)]) -> Result<Attributed, String> {
    let mut tried = Vec::new();
    for (label, path) in candidates {
        if !path.exists() {
            tried.push(format!("{label} ({}) does not exist", path.display()));
            continue;
        }
        let facts = on_disk(path);
        match &facts.cdhash {
            Some(h) if h.eq_ignore_ascii_case(running_cdhash) => {
                return Ok(Attributed { facts, source: path.clone() })
            }
            Some(h) => tried.push(format!(
                "{label} ({}) has cdhash {h}, which is NOT the running image's",
                path.display()
            )),
            None => tried.push(format!(
                "{label} ({}) has no readable cdhash",
                path.display()
            )),
        }
    }
    Err(format!(
        "no file on disk carries the running image's cdhash, so its authority and designated \
         requirement cannot be read from anywhere: {}",
        tried.join("; ")
    ))
}

// ──────────────────────────────────────────────────────────────────────────
//  Build metadata, compiled into THIS image
// ──────────────────────────────────────────────────────────────────────────

/// Unix seconds at which this image was compiled, or empty when `build.rs`
/// could not determine it. Emitted by `lib/threads/build.rs`.
const BUILD_EPOCH: &str = env!("THREADS_BUILD_EPOCH");
/// The commit this image was built from, or empty. Emitted by `build.rs`.
const BUILD_COMMIT: &str = env!("THREADS_BUILD_COMMIT");
/// `"1"` / `"0"` / empty — whether the working tree had uncommitted compiled
/// inputs at build time.
///
/// **Empty now means only one thing: `git` could not be run.** It used to also
/// mean "the tree was clean", because `build.rs`'s git helper folded empty
/// stdout into its failure case and `git status --porcelain` prints nothing for
/// a clean tree — so every clean build in this daemon's history reported
/// `commit_dirty` as `known: false`. The single field that would have caught a
/// stale [`BUILD_COMMIT`] was unreadable by construction.
const BUILD_DIRTY: &str = env!("THREADS_BUILD_DIRTY");
/// How [`BUILD_COMMIT`] was obtained: `"pin"`, `"git"`, or empty.
///
/// This is the difference between a promise and a guess, and it is reported
/// because the two are not interchangeable:
///
/// * `"pin"` — passed in by whoever decided to ship this commit, via
///   `THREADS_BUILD_COMMIT_PIN`. Cargo's fingerprint covers the *value*, so this
///   image cannot be a leftover build carrying somebody else's commit.
/// * `"git"` — `build.rs` asked `git` at build time. Correct as far as mtimes
///   can establish, and the only thing a plain `cargo build` can offer, but it
///   is a proxy and may lag a ref that moved without disturbing a watched file.
/// * empty — no commit was recorded at all.
///
/// `promote` requires `"pin"` and refuses to publish anything else, so nothing
/// reaching the deploy path carries a guess.
const BUILD_PROVENANCE: &str = env!("THREADS_BUILD_PROVENANCE");

/// The package directories `build.rs` counted as compiled inputs when it decided
/// [`BUILD_DIRTY`] — comma-separated, repo-root-relative, empty when the scope
/// could not be established (in which case [`BUILD_DIRTY`] is empty too).
///
/// # Why a scope is PUBLISHED rather than kept inside the build script
///
/// A dirt flag is only as good as the set of files it looked at, and that set
/// used to be invisible: `build.rs` matched any `.rs`/`.sql`/`.toml`/`.lock`
/// path in the entire repository while its own comment claimed it was scoped to
/// what enters the product. Nothing could contradict the comment, because
/// nothing outside the build script could see what the check actually did — and
/// so for a long time an agent's `.pen.lock` workspace lease was enough to make
/// this image report itself as having no version identity at all.
///
/// Publishing the scope makes it testable from outside. `tests/build_provenance.rs`
/// asserts it against the crate's real dependency closure, parsed with a real
/// TOML parser, which is the one check that can catch the build script's
/// dependency-line scan silently MISSING a crate whose bytes ship inside this
/// binary — the failure direction that would make [`build_commit`] name a commit
/// for bytes that commit does not describe.
pub const BUILD_DIRT_SCOPE: &str = env!("THREADS_BUILD_DIRT_SCOPE");

fn build_json() -> Value {
    let built_at = match BUILD_EPOCH.trim().parse::<i64>() {
        Ok(secs) => Fact::of(
            chrono::DateTime::from_timestamp(secs, 0).map(|d| d.to_rfc3339()),
            format!("build.rs recorded the epoch second {secs}, which is not a valid timestamp"),
        ),
        Err(_) => Fact::unknown(
            "this image carries no build timestamp — it was compiled before \
             lib/threads/build.rs existed, or build.rs could not read a clock",
        ),
    };
    let commit = Fact::of(
        Some(BUILD_COMMIT.trim()).filter(|s| !s.is_empty()),
        "this image records no commit — it was compiled before lib/threads/build.rs existed, \
         or git was not available in the build environment",
    );
    let dirty = match BUILD_DIRTY.trim() {
        "1" => Fact::known(true),
        "0" => Fact::known(false),
        _ => Fact::unknown(
            "git could not be run at build time, so whether the commit above fully describes \
             this image is unknown. Note that a CLEAN tree is reported as `false`, not as this \
             — the two were indistinguishable until build.rs stopped folding git's empty \
             stdout into its failure case",
        ),
    };
    let provenance = match BUILD_PROVENANCE.trim() {
        "pin" => Fact::known("pin"),
        "git" => Fact::known("git"),
        _ => Fact::unknown(
            "this image records no provenance for its commit — it was compiled before build.rs \
             distinguished a pinned commit from a sniffed one, so the commit above could be \
             either",
        ),
    };
    json!({
        "built_at": built_at.json(),
        "commit": commit.json(),
        "commit_dirty": dirty.json(),
        "commit_provenance": provenance.json(),
        "source": "compiled into this image by lib/threads/build.rs — it describes the running \
                   image, not any file on disk",
    })
}

/// One machine-readable line naming the commit compiled into THIS image, how it
/// was obtained, and whether the tree was dirty — printed by
/// `threads --build-stamp`.
///
/// # Why a binary has to be able to say this about itself
///
/// `promote` signs whatever `cargo build` leaves at the product path, and cargo
/// relinks nothing when its fingerprint is already satisfied. So "I ran a build
/// and then signed the result" is not evidence that the signed bytes came from
/// the commit being promoted — the bytes may predate it entirely. The only thing
/// that can settle it is the image itself, read back after the build, which is
/// why this is a flag on the binary and not a fact `promote` computes.
///
/// Deliberately a flat `k=v` line rather than JSON: it is read by a shell script
/// in front of a human, it is parsed before any config is resolved or any file is
/// opened, and it must not acquire a dependency on a serialiser to do it.
/// The commit this image was built from, **only when that commit names these
/// bytes** — `None` otherwise.
///
/// This is the version identity, and it is deliberately narrower than the
/// private `BUILD_COMMIT` it reads. A commit names a source tree; it names an
/// *artifact* only
/// when the tree it was built from held nothing the commit does not describe.
/// So three cases collapse into `None`:
///
/// * no commit recorded — compiled before `build.rs` existed, or no git;
/// * `dirty=1` — the tree carried uncommitted compiled inputs, so these bytes
///   are not the commit's bytes and saying they are would be a lie;
/// * `dirty` unknown — `git` could not be run, which is *not* the same as clean
///   (see `BUILD_DIRTY`'s own comment) and must not be read as it.
///
/// Returning `None` rather than a best guess is what lets callers treat silence
/// as silence. A caller comparing two images must not read "I cannot say" as
/// "different", and must not read it as "same" either; [`Option`] forces it to
/// choose a behaviour for the unknown case explicitly.
pub fn build_commit() -> Option<&'static str> {
    if BUILD_DIRTY.trim() != "0" {
        return None;
    }
    Some(BUILD_COMMIT.trim()).filter(|s| !s.is_empty())
}

pub fn build_stamp_line() -> String {
    let commit = BUILD_COMMIT.trim();
    let provenance = BUILD_PROVENANCE.trim();
    let dirty = BUILD_DIRTY.trim();
    format!(
        "commit={} provenance={} dirty={} epoch={}",
        if commit.is_empty() { "unknown" } else { commit },
        if provenance.is_empty() { "unknown" } else { provenance },
        match dirty {
            "1" => "1",
            "0" => "0",
            _ => "unknown",
        },
        BUILD_EPOCH.trim(),
    )
}

// ──────────────────────────────────────────────────────────────────────────
//  The process's own start time
// ──────────────────────────────────────────────────────────────────────────

/// When the ORIGINAL process was launched, from the kernel.
///
/// **This is not the age of the running image.** [`crate::update`] re-execs in
/// place, which keeps the pid and therefore keeps this timestamp. A daemon that
/// has absorbed four builds still reports the launch of the first. Reading this
/// as the daemon's age is the specific error that made an investigation
/// conclude four days of shipped commits had never run.
#[cfg(target_os = "macos")]
fn process_started_at() -> Result<String, String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let rc = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
            size as libc::c_int,
        )
    };
    if rc <= 0 || rc as usize != size {
        return Err(format!(
            "proc_pidinfo(PROC_PIDTBSDINFO) returned {rc}: {}",
            std::io::Error::last_os_error()
        ));
    }
    chrono::DateTime::from_timestamp(info.pbi_start_tvsec as i64, 0)
        .map(|d| d.to_rfc3339())
        .ok_or_else(|| format!("the kernel reported start second {}", info.pbi_start_tvsec))
}

#[cfg(not(target_os = "macos"))]
fn process_started_at() -> Result<String, String> {
    Err("process start time is only implemented for macOS in this daemon".into())
}

// ──────────────────────────────────────────────────────────────────────────
//  The cheap divergence check, for `daemon.status`
// ──────────────────────────────────────────────────────────────────────────

/// Does the launch path still hold the image this process is executing?
///
/// Two syscalls, no subprocess — cheap enough for `daemon.status`, which the
/// TUI polls. `None` means the question could not be answered, and a caller
/// must render that as unknown rather than as `true`.
///
/// This is what replaced `daemon.status`'s old `binary` field. That field
/// reported a PATH and implied the running image; by the time three
/// investigations read it, the file at that path had been replaced by somebody
/// else's `cargo build` and it was pointing them at an entirely different,
/// unsigned binary.
pub fn launch_path_holds_running_image(launch_path: Option<&Path>) -> Option<bool> {
    let path = launch_path?;
    let running = text_ident_of_self()?;
    let on_path = path_ident(path)?;
    Some(running == on_path)
}

/// `(device, inode)` — the pair, not the inode alone. An inode is only unique
/// within a filesystem, and the launch path and the running image are not
/// guaranteed to be on the same one.
#[cfg(target_os = "macos")]
fn text_ident_of_self() -> Option<(u64, u64)> {
    text_vnode::of_self().ok().map(|v| (v.device as u64, v.inode))
}

#[cfg(not(target_os = "macos"))]
fn text_ident_of_self() -> Option<(u64, u64)> {
    None
}

fn path_ident(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.dev() as u64, m.ino()))
}

// ──────────────────────────────────────────────────────────────────────────
//  The report
// ──────────────────────────────────────────────────────────────────────────

/// What the report needs to know that only the caller has.
pub struct Context<'a> {
    /// `current_exe()` — the path this process was LAUNCHED from. It is not
    /// necessarily where the running image still lives.
    pub launch_path: Option<&'a Path>,
    /// The daemon's data directory, which holds `threads.last-good` and
    /// `restarts.jsonl`.
    pub data_dir: &'a Path,
    /// [`crate::update::UpdateGate::boot_identity`] — the identity this process
    /// recorded for itself at boot.
    pub boot_identity: Option<&'a str>,
    /// [`crate::runtime::ThreadManager::boot_id`] — the per-process UUID.
    pub boot_id: Option<&'a str>,
    /// [`crate::runtime::ThreadManager::started_at`] — when THIS IMAGE began
    /// serving.
    pub image_booted_at: Option<&'a str>,
}

/// **The report.** Everything known about the image this process is executing.
///
/// Call this from the process you are asking about. It reports on *itself* —
/// there is no pid parameter, deliberately, because a report that could be
/// pointed at the wrong process is the same class of instrument this module
/// replaces.
pub fn report(cx: &Context) -> Value {
    let pid = std::process::id();

    // ── the kernel's account of the running image ────────────────────────
    #[cfg(target_os = "macos")]
    let (cdhash_res, status_res, ident_res, team_res, blob_res) = {
        let p = pid as libc::pid_t;
        (
            kernel::cdhash(p),
            kernel::status(p),
            kernel::identity(p),
            kernel::team_id(p),
            kernel::blob(p),
        )
    };
    #[cfg(not(target_os = "macos"))]
    let (cdhash_res, status_res, ident_res, team_res, blob_res) = {
        let e = || Err("code signing identity is only readable on macOS".to_string());
        (e(), e(), e(), e(), e().map(|_: String| Vec::new()))
    };

    let running_cdhash_hex = cdhash_res.as_ref().ok().cloned();
    // Rendered in the SAME `cdhash:` form `update::binary_identity` and
    // `restarts.jsonl` use, so a reader can compare them without reformatting
    // — comparing a bare hex against a `cdhash:` prefixed string is how a
    // comparison silently finds nothing.
    let running_identity = running_cdhash_hex.as_ref().map(|h| format!("cdhash:{h}"));

    let sig_facts = blob_res.as_ref().ok().map(|b| parse_superblob(b));

    let ad_hoc = match (&blob_res, &sig_facts) {
        (Ok(_), Some(Ok(f))) => Fact::known(f.ad_hoc),
        (Ok(_), Some(Err(e))) => Fact::unknown(format!(
            "the kernel returned this process's signature blob but it could not be parsed, so \
             whether it is ad-hoc is unknown — it is NOT therefore unsigned: {e}"
        )),
        (Err(e), _) => Fact::unknown(e.clone()),
        _ => Fact::unknown("the signature blob was not read"),
    };
    let properly_signed = match &sig_facts {
        Some(Ok(f)) => Fact::known(f.properly_signed()),
        _ => Fact::unknown(
            ad_hoc
                .json()
                .get("why")
                .and_then(Value::as_str)
                .unwrap_or("the signature blob was not read")
                .to_string(),
        ),
    };
    let cms_len = match &sig_facts {
        Some(Ok(f)) => Fact::known(f.cms_len),
        _ => Fact::unknown("the signature blob was not read"),
    };
    let linker_signed = match &sig_facts {
        Some(Ok(f)) => Fact::known(f.linker_signed),
        _ => Fact::unknown("the signature blob was not read"),
    };
    let cd_flags = match &sig_facts {
        Some(Ok(f)) => Fact::known(format!("{:#010x}", f.cd_flags)),
        _ => Fact::unknown("the signature blob was not read"),
    };

    let running_identifier = Fact::of(ident_res.clone().ok(), match &ident_res {
        Err(e) => e.clone(),
        Ok(_) => String::new(),
    });

    // ── the text vnode ───────────────────────────────────────────────────
    #[cfg(target_os = "macos")]
    let text = text_vnode::of_self();
    #[cfg(not(target_os = "macos"))]
    let text: Result<text_vnode_stub::TextVnode, String> =
        Err("the executing text vnode is only readable on macOS".into());

    #[cfg(target_os = "macos")]
    let (text_inode, text_kernel_path) = match &text {
        Ok(v) => (
            Fact::known(v.inode),
            Fact::known(v.kernel_path.clone()),
        ),
        Err(e) => (Fact::unknown(e.clone()), Fact::unknown(e.clone())),
    };
    #[cfg(not(target_os = "macos"))]
    let (text_inode, text_kernel_path) = {
        let e = "the executing text vnode is only readable on macOS".to_string();
        (Fact::unknown(e.clone()), Fact::unknown(e))
    };

    // ── the launch path, as it stands RIGHT NOW ──────────────────────────
    let launch_path_str = cx.launch_path.map(|p| p.display().to_string());
    let launch_inode = cx
        .launch_path
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        });
    let launch_on_disk = cx.launch_path.map(on_disk);
    let launch_cdhash = launch_on_disk.as_ref().and_then(|d| d.cdhash.clone());

    let no_launch_path = "this process has no launch path — `current_exe()` did not resolve, \
                          which is normal in a test harness";

    // ── attribution, for the facts only a file can supply ────────────────
    let last_good = cx.data_dir.join(crate::update::LAST_GOOD);
    let attributed = match &running_cdhash_hex {
        Some(h) => {
            let mut candidates: Vec<(&str, PathBuf)> = Vec::new();
            if let Some(p) = cx.launch_path {
                candidates.push(("the launch path", p.to_path_buf()));
            }
            candidates.push(("the last-good copy", last_good));
            attribute(h, &candidates)
        }
        None => Err(
            "the kernel would not report this process's cdhash, so no file on disk can be \
             matched to the running image"
                .into(),
        ),
    };

    let (authority, designated_requirement, signed_for_deploy, attribution_json) = match &attributed
    {
        Ok(a) => (
            Fact::of(
                a.facts.authority.clone(),
                format!(
                    "{} carries the running image's code, but `codesign` reported no Authority \
                     line for it — which is what an ad-hoc signature looks like",
                    a.source.display()
                ),
            ),
            Fact::of(
                a.facts.designated_requirement.clone(),
                format!(
                    "{} carries the running image's code but `codesign` reported no designated \
                     requirement",
                    a.source.display()
                ),
            ),
            Fact::known(a.facts.signed_for_deploy),
            json!({
                "status": "attributed",
                "source": a.source.display().to_string(),
                "how": "this file's cdhash is identical to the kernel's cdhash for this process. \
                        Code signing is content-addressed, so an identical cdhash means an \
                        identical code directory: this file's signature IS the running image's \
                        signature.",
            }),
        ),
        Err(why) => (
            Fact::unknown(why.clone()),
            Fact::unknown(why.clone()),
            Fact::unknown(format!(
                "{why}. NOTE: this is NOT a report that the running image is unsigned — see \
                 `running.code_signature.properly_signed`, which the kernel answered directly."
            )),
            json!({
                "status": "unattributable",
                "source": Value::Null,
                "how": why,
            }),
        ),
    };

    // ── the verdict ──────────────────────────────────────────────────────
    let identifier_is_deploy_identifier = running_identifier
        .as_str()
        .map(|s| s == STABLE_BUNDLE_ID);
    let verdict = match (properly_signed.value().and_then(Value::as_bool), identifier_is_deploy_identifier) {
        (Some(true), Some(true)) => Fact::known("signed-for-deploy"),
        (Some(true), Some(false)) => Fact::known("signed-but-not-as-the-deploy-identifier"),
        (Some(false), _) => Fact::known("ad-hoc"),
        (None, _) => Fact::unknown(
            "the running image's signature could not be read from the kernel, so it is neither \
             signed nor unsigned as far as this report is concerned",
        ),
        (Some(true), None) => Fact::unknown(
            "the running image is properly signed, but the kernel would not report its signing \
             identifier, so whether it is signed AS THE DEPLOY IDENTITY is unknown",
        ),
    };

    // ── divergence ───────────────────────────────────────────────────────
    // Compared as (device, inode) pairs: an inode is only unique within a
    // filesystem, and nothing guarantees the launch path and the running image
    // are on the same one.
    let inode_same = match (text_ident_of_self(), cx.launch_path.and_then(path_ident)) {
        (Some(a), Some(b)) => Fact::known(a == b),
        _ => Fact::unknown(
            "one of the two inodes could not be read, so they cannot be compared — this is NOT \
             a report that they match",
        ),
    };
    let identity_same = match (&running_cdhash_hex, &launch_cdhash) {
        (Some(a), Some(b)) => Fact::known(a.eq_ignore_ascii_case(b)),
        _ => Fact::unknown(
            "one of the two cdhashes could not be read, so they cannot be compared — this is \
             NOT a report that they match",
        ),
    };
    // Diverged if EITHER signal says so. Two independent instruments; if they
    // disagree, that is itself worth surfacing rather than picking a winner.
    let diverged = match (
        inode_same.value().and_then(Value::as_bool),
        identity_same.value().and_then(Value::as_bool),
    ) {
        (Some(a), Some(b)) => Fact::known(!a || !b),
        (Some(a), None) => Fact::known(!a),
        (None, Some(b)) => Fact::known(!b),
        (None, None) => Fact::unknown(
            "neither the inodes nor the cdhashes could be compared, so whether the launch path \
             still holds the running image is unknown",
        ),
    };
    let divergence_meaning = match diverged.value().and_then(Value::as_bool) {
        Some(true) =>
            "THE FILE AT THE LAUNCH PATH IS NOT THE RUNNING IMAGE. It was replaced underneath \
             this running process and the replacement has not been absorbed. Anything you learn \
             by inspecting that path describes a DIFFERENT binary than the one answering you.",
        Some(false) =>
            "the launch path still holds the running image, so facts derived from that path do \
             describe what is running",
        None => "undeterminable — see `diverged.why`",
    };

    // ── would the on-disk image be absorbed? ─────────────────────────────
    let on_disk_signed = launch_on_disk.as_ref().and_then(|d| d.signed_for_deploy());
    let differs = diverged.value().and_then(Value::as_bool);
    let (would_absorb, absorb_reason) = match (on_disk_signed, differs) {
        (Some(true), Some(true)) => (
            Fact::known(true),
            format!(
                "the image at the launch path is signed for deploy (its designated requirement \
                 names {STABLE_BUNDLE_ID}) and differs from the running one, so the watcher will \
                 absorb it at the next quiet moment"
            ),
        ),
        (Some(true), Some(false)) => (
            Fact::known(false),
            "the image at the launch path is signed for deploy, but it is the image already \
             running — there is nothing to absorb"
                .to_string(),
        ),
        (Some(false), _) => (
            Fact::known(false),
            format!(
                "the image at the launch path is NOT signed for deploy — its designated \
                 requirement does not name {STABLE_BUNDLE_ID} — so the watcher will NEVER absorb \
                 it, no matter how long it sits there. A plain `cargo build` leaves an ad-hoc \
                 image and that is deliberately not a deploy; it ships through \
                 `promote publish threads`."
            ),
        ),
        (Some(true), None) => (
            Fact::unknown(
                "the image at the launch path is signed for deploy, but whether it DIFFERS from \
                 the running image could not be determined, so whether there is anything to \
                 absorb is unknown",
            ),
            "the image at the launch path is signed for deploy, but divergence from the running \
             image is undeterminable — see `divergence.diverged.why`"
                .to_string(),
        ),
        (None, _) => {
            // Either there is no launch path at all, or there is no FILE at
            // it. Both are unanswerable, and they are different situations, so
            // say which.
            let why = match (&launch_path_str, launch_on_disk.as_ref().map(|d| d.exists)) {
                (Some(p), Some(false)) => format!(
                    "there is no file at the launch path ({p}) any more, so there is nothing \
                     that could be absorbed and nothing to inspect"
                ),
                _ => no_launch_path.to_string(),
            };
            (Fact::unknown(why.clone()), why)
        }
    };

    // ── boot ─────────────────────────────────────────────────────────────
    let proc_started = process_started_at();
    let image_booted = cx.image_booted_at.map(str::to_string);
    let re_exec = match (&proc_started, &image_booted) {
        (Ok(p), Some(i)) => {
            match (
                chrono::DateTime::parse_from_rfc3339(p),
                chrono::DateTime::parse_from_rfc3339(i),
            ) {
                // 5s of slack absorbs the ordinary gap between the kernel
                // stamping the process and this daemon building its manager.
                (Ok(p), Ok(i)) => Fact::known((i - p).num_seconds() > 5),
                _ => Fact::unknown("one of the two timestamps did not parse as RFC3339"),
            }
        }
        _ => Fact::unknown("the process start time or the image boot time is unknown"),
    };

    let boot_identity_matches = match (cx.boot_identity, &running_identity) {
        (Some(b), Some(r)) => Fact::known(b == r),
        (None, _) => Fact::unknown(
            "this process recorded no boot identity — `UpdateGate` only computes one when the \
             running executable is the daemon binary, so a test harness or a CLI has none",
        ),
        (_, None) => Fact::unknown("the kernel would not report this process's cdhash"),
    };

    json!({
        "answers": "which image is this process actually executing, and is the file it was \
                    launched from still that image",

        // ── THE RUNNING IMAGE ────────────────────────────────────────────
        "running": {
            "pid": pid,
            "observed_from": "the kernel, for this pid — csops(2) and proc_pidinfo(2). None of \
                              these fields is read from a path, so replacing the file at the \
                              launch path cannot change them.",
            "code_signature": {
                "cdhash": Fact::of(running_identity.clone(), match &cdhash_res {
                    Err(e) => e.clone(),
                    Ok(_) => String::new(),
                }).json(),
                "identifier": running_identifier.json(),
                "team_id": Fact::of(team_res.clone().ok(), match &team_res {
                    Err(e) => format!(
                        "{e}. A self-signed certificate such as `harness-codesign` carries no \
                         team identifier, so this being absent is normal and is not a defect."
                    ),
                    Ok(_) => String::new(),
                }).json(),
                "verdict": verdict.json(),
                "properly_signed": properly_signed.json(),
                "ad_hoc": ad_hoc.json(),
                "linker_signed": linker_signed.json(),
                "cms_signature_bytes": cms_len.json(),
                "code_directory_flags": cd_flags.json(),
                "signed_for_deploy": signed_for_deploy.json(),
                "identifier_is_the_deploy_identifier": Fact::of(
                    identifier_is_deploy_identifier,
                    "the kernel would not report this process's signing identifier",
                ).json(),
                "deploy_identifier": STABLE_BUNDLE_ID,
                "authority": authority.json(),
                "designated_requirement": designated_requirement.json(),
                "attribution": attribution_json,
                "kernel_status_flags": Fact::of(
                    status_res.as_ref().ok().map(|s| format!("{s:#010x}")),
                    match &status_res { Err(e) => e.clone(), Ok(_) => String::new() },
                ).json(),
                "kernel_status_flags_caveat": "reported for completeness and NOT used to decide \
                                               ad-hoc-ness: measured on macOS 25.6, an ad-hoc \
                                               process and a properly signed one both report \
                                               CS_ADHOC clear. `ad_hoc` above comes from the \
                                               CodeDirectory in the signature blob, which does \
                                               distinguish them.",
            },
            "text_inode": text_inode.json(),
            "text_inode_source": "the kernel VM region backing this image's __TEXT segment \
                                  (proc_pidinfo PROC_PIDREGIONPATHINFO, anchored on a function \
                                  address in this binary). A file can be replaced under a running \
                                  process; the inode it is executing cannot.",
            "kernel_text_path": text_kernel_path.json(),
            "kernel_text_path_caveat": "the name the kernel still holds for that vnode. It is NOT \
                                        evidence: an unlinked vnode keeps its old name, so this \
                                        string reads identically whether or not the file was \
                                        replaced. The inode is the evidence.",
            "build": build_json(),
        },

        // ── THE FILE AT THE LAUNCH PATH, RIGHT NOW ───────────────────────
        "launch_path": {
            "path": Fact::of(launch_path_str.clone(), no_launch_path).json(),
            "path_source": "std::env::current_exe() — the path this process was LAUNCHED from. \
                            It is NOT a statement about where the running image lives now.",
            "inode": Fact::of(launch_inode, format!(
                "the launch path could not be stat'ed{}",
                if launch_path_str.is_none() { " (there is none)" } else { "" }
            )).json(),
            "caveat": "EVERY FIELD IN THIS OBJECT IS PATH-DERIVED and describes whatever file is \
                       at that path at this instant — which may be a different binary from the \
                       one answering this request. Read `divergence` before trusting any of it.",
            "code_signature": match &launch_on_disk {
                Some(d) => json!({
                    "cdhash": Fact::of(
                        d.cdhash.as_ref().map(|h| format!("cdhash:{h}")),
                        "codesign reported no CDHash for the file at the launch path",
                    ).json(),
                    "identifier": Fact::of(
                        d.identifier.clone(),
                        "codesign reported no Identifier for the file at the launch path",
                    ).json(),
                    "ad_hoc": Fact::of(
                        d.ad_hoc,
                        "codesign reported no CodeDirectory flags for the file at the launch path",
                    ).json(),
                    "authority": Fact::of(
                        d.authority.clone(),
                        "codesign reported no Authority for the file at the launch path — which \
                         is what an ad-hoc signature looks like",
                    ).json(),
                    "designated_requirement": Fact::of(
                        d.designated_requirement.clone(),
                        "codesign reported no designated requirement for the file at the launch \
                         path",
                    ).json(),
                    "signed_for_deploy": Fact::of(
                        d.signed_for_deploy(),
                        "there is no file at the launch path, so whether it is signed for \
                         deploy is not a question that has an answer",
                    ).json(),
                    "exists": d.exists,
                }),
                None => Value::Null,
            },
        },

        // ── DIVERGENCE — the condition that confused three investigations ─
        "divergence": {
            "diverged": diverged.json(),
            "meaning": divergence_meaning,
            "by_inode": {
                "running_text_inode": text_inode.json(),
                "launch_path_inode": Fact::of(launch_inode, "the launch path could not be stat'ed").json(),
                "same": inode_same.json(),
                "why_this_is_the_test": "a path can be replaced underneath a running process; the \
                                         inode it is executing cannot. These two numbers differing \
                                         IS the replacement, stated rather than inferred.",
            },
            "by_code_identity": {
                "running_cdhash": Fact::of(running_identity.clone(), "the kernel would not report this process's cdhash").json(),
                "launch_path_cdhash": Fact::of(
                    launch_cdhash.as_ref().map(|h| format!("cdhash:{h}")),
                    "codesign reported no CDHash for the file at the launch path",
                ).json(),
                "same": identity_same.json(),
            },
        },

        // ── IS SOMETHING WAITING TO DEPLOY, AND WHY ISN'T IT ─────────────
        "absorbable": {
            "would_absorb": would_absorb.json(),
            "reason": absorb_reason,
            "on_disk_signed_for_deploy": Fact::of(
                on_disk_signed,
                no_launch_path,
            ).json(),
            "differs_from_running": diverged.json(),
        },

        // ── BOOT ─────────────────────────────────────────────────────────
        "boot": {
            "image_booted_at": Fact::of(
                image_booted.clone(),
                "the caller supplied no image boot time",
            ).json(),
            "image_booted_at_means": "when THIS IMAGE started serving. The daemon re-execs in \
                                      place and KEEPS ITS PID, so this is the number that tracks \
                                      the code, and it is not the process's age.",
            "process_started_at": Fact::of(
                proc_started.as_ref().ok().cloned(),
                match &proc_started { Err(e) => e.clone(), Ok(_) => String::new() },
            ).json(),
            "process_started_at_means": "when the ORIGINAL process was launched, from the kernel. \
                                         It SURVIVES EVERY RE-EXEC, so it is not the age of the \
                                         running image. Reading it as such — which is what `ps` \
                                         elapsed time shows — is the error that made an \
                                         investigation conclude four days of shipped commits had \
                                         never run.",
            "re_exec_since_process_start": re_exec.json(),
            "boot_id": {
                "value": Fact::of(
                    cx.boot_id.map(str::to_string),
                    "the caller supplied no boot id",
                ).json(),
                "recorded_where": "IN MEMORY ONLY. It is a fresh UUID minted per process in \
                                   ThreadManager::new, carried on the WS Welcome frame and \
                                   written into thread-lock rows in threads.db while a lock is \
                                   held. IT IS NOT WRITTEN TO restarts.jsonl and does not persist \
                                   anywhere you can grep for it afterwards — a boot id quoted in \
                                   a report will correctly appear nowhere on disk. It is NOT the \
                                   same thing as `boot_identity` below.",
            },
            "boot_identity": {
                "value": Fact::of(
                    cx.boot_identity.map(str::to_string),
                    "this process recorded no boot identity — UpdateGate only computes one when \
                     the running executable is the daemon binary",
                ).json(),
                "recorded_where": "<data-dir>/restarts.jsonl, as \
                                   {\"event\":\"boot\",\"identity\":\"cdhash:…\"}. THIS is the \
                                   boot value that is on disk and greppable.",
                "matches_running_image": boot_identity_matches.json(),
                "matches_running_image_means": "false here would mean the identity this process \
                                                recorded at boot is not the identity the kernel \
                                                says it is executing, which should be impossible \
                                                and would indicate the boot record was computed \
                                                from a path that had already been replaced.",
            },
            "restart_log": cx.data_dir.join(crate::update::RESTART_LOG).display().to_string(),
        },
    })
}

/// The report a **client** gets when it could not reach the daemon.
///
/// Same shape, but every `running.*` fact is undeterminable with the reason,
/// because a process that is not answering has no observable running image.
/// The disk half is still filled in — those facts are process-independent and
/// are exactly what somebody wants when the daemon is suspect.
///
/// This exists so the CLI never reports on ITSELF by accident. A CLI that
/// computed `running.*` locally would be describing the CLI process, which is
/// the same category of error as the `binary` field this endpoint replaced.
pub fn unreachable_report(cx: &Context, why: &str) -> Value {
    // Short, because it is repeated on every field. The full reason is carried
    // once, at `unreachable_why`.
    let unknown = "the daemon was not reachable, so no running image was observable".to_string();
    let f = || Fact::unknown(unknown.clone()).json();

    let launch_path_str = cx.launch_path.map(|p| p.display().to_string());
    let launch_inode = cx
        .launch_path
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        });
    let launch_on_disk = cx.launch_path.map(on_disk);

    json!({
        "answers": "which image is this process actually executing, and is the file it was \
                    launched from still that image",
        "reachable": false,
        "unreachable_why": why,
        "running": {
            "pid": Value::Null,
            "observed_from": unknown,
            "code_signature": {
                "cdhash": f(), "identifier": f(), "team_id": f(), "verdict": f(),
                "properly_signed": f(), "ad_hoc": f(), "linker_signed": f(),
                "cms_signature_bytes": f(), "code_directory_flags": f(),
                "signed_for_deploy": f(), "identifier_is_the_deploy_identifier": f(),
                "deploy_identifier": STABLE_BUNDLE_ID,
                "authority": f(), "designated_requirement": f(),
                "attribution": json!({ "status": "unattributable", "source": Value::Null, "how": unknown }),
                "kernel_status_flags": f(),
            },
            "text_inode": f(),
            "kernel_text_path": f(),
            "build": {
                "built_at": f(), "commit": f(), "commit_dirty": f(),
                "source": unknown,
            },
        },
        "launch_path": {
            "path": Fact::of(
                launch_path_str,
                "no launch path was supplied and current_exe() did not resolve, so there is no \
                 file to report on",
            ).json(),
            // Said plainly, because getting this wrong is the entire bug class.
            // With the daemon silent there is nothing that can tell us where it
            // was launched from, so this is the CALLER'S OWN executable — the
            // right guess when `threads image` is the same binary as the
            // daemon, and the wrong one otherwise. It is labelled, not assumed.
            "path_source": "THE CALLING PROCESS'S OWN EXECUTABLE (current_exe()). With the \
                            daemon unreachable, nothing can report where it was actually \
                            launched from, so this is a guess that is correct only if you \
                            invoked the same binary the daemon runs. It is NOT evidence about \
                            the daemon.",
            "inode": Fact::of(launch_inode, "the launch path could not be stat'ed").json(),
            "caveat": "EVERY FIELD IN THIS OBJECT IS PATH-DERIVED. With the daemon unreachable \
                       there is nothing to compare it against, so it says what is ON DISK and \
                       nothing about what is running.",
            "code_signature": match &launch_on_disk {
                Some(d) => json!({
                    "cdhash": Fact::of(
                        d.cdhash.as_ref().map(|h| format!("cdhash:{h}")),
                        "codesign reported no CDHash for the file at that path (it may not \
                         exist, or may not be a signed Mach-O)",
                    ).json(),
                    "identifier": Fact::of(
                        d.identifier.clone(),
                        "codesign reported no Identifier for the file at that path",
                    ).json(),
                    "ad_hoc": Fact::of(
                        d.ad_hoc,
                        "codesign reported no CodeDirectory flags for the file at that path, so \
                         whether it is ad-hoc is unknown — it is NOT therefore properly signed",
                    ).json(),
                    "authority": Fact::of(
                        d.authority.clone(),
                        "codesign reported no Authority for the file at that path — which is \
                         what an ad-hoc signature looks like",
                    ).json(),
                    "designated_requirement": Fact::of(
                        d.designated_requirement.clone(),
                        "codesign reported no designated requirement for the file at that path",
                    ).json(),
                    "signed_for_deploy": Fact::of(
                        d.signed_for_deploy(),
                        "there is no file at that path, so whether it is signed for deploy is \
                         not a question that has an answer",
                    ).json(),
                    "exists": d.exists,
                }),
                None => Value::Null,
            },
        },
        "divergence": {
            "diverged": f(),
            "meaning": "undeterminable — with the daemon unreachable there is no running image \
                        to compare the launch path against. This is NOT a report that they match.",
            "by_inode": {
                "running_text_inode": f(),
                "launch_path_inode": Fact::of(launch_inode, "the launch path could not be stat'ed").json(),
                "same": f(),
            },
            "by_code_identity": {
                "running_cdhash": f(),
                "launch_path_cdhash": Fact::of(
                    launch_on_disk.as_ref().and_then(|d| d.cdhash.as_ref()).map(|h| format!("cdhash:{h}")),
                    "codesign reported no CDHash for the file at that path",
                ).json(),
                "same": f(),
            },
        },
        "absorbable": {
            // The on-disk half of this IS answerable without the daemon, and
            // it is the more useful half: "is what is sitting there even
            // capable of deploying".
            "would_absorb": f(),
            "reason": "whether the on-disk image WOULD be absorbed depends on it differing from \
                       the running one, and the running one is unobservable. Its signing state, \
                       below, is answerable and is the half that usually explains a stuck deploy.",
            "on_disk_signed_for_deploy": Fact::of(
                launch_on_disk.as_ref().and_then(|d| d.signed_for_deploy()),
                "there is no file at that path (or none was supplied), so whether it is signed \
                 for deploy is not a question that has an answer",
            ).json(),
            "differs_from_running": f(),
        },
        "boot": {
            "image_booted_at": f(),
            "process_started_at": f(),
            "re_exec_since_process_start": f(),
            "boot_id": { "value": f(), "recorded_where": "in memory only — see the daemon's own report" },
            "boot_identity": {
                "value": f(),
                "recorded_where": "<data-dir>/restarts.jsonl",
                "matches_running_image": f(),
            },
            "restart_log": cx.data_dir.join(crate::update::RESTART_LOG).display().to_string(),
        },
    })
}

// ──────────────────────────────────────────────────────────────────────────
//  Rendering — shared by the CLI so the two cannot drift
// ──────────────────────────────────────────────────────────────────────────

fn fv<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    let mut cur = v;
    for k in path {
        cur = match cur.get(k) {
            Some(x) => x,
            None => return &Value::Null,
        };
    }
    cur
}

/// Render a [`Fact`] the way a human reading a terminal needs it: the value, or
/// the word `undeterminable` followed by the reason. Never a blank.
fn show(v: &Value) -> String {
    if v.get("known").and_then(Value::as_bool) == Some(true) {
        match v.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(x) => x.to_string(),
            None => "undeterminable".into(),
        }
    } else {
        let why = v.get("why").and_then(Value::as_str).unwrap_or("no reason given");
        format!("UNDETERMINABLE — {why}")
    }
}

/// The human-readable report. Built from the SAME payload the WS method
/// returns, so the two can never describe different states.
pub fn render(v: &Value) -> String {
    let mut o = String::new();
    let p = |o: &mut String, k: &str, val: String| {
        o.push_str(&format!("  {k:<26} {val}\n"));
    };

    if v.get("reachable").and_then(Value::as_bool) == Some(false) {
        o.push_str(&format!(
            "!! THE DAEMON WAS NOT REACHABLE — {}\n\
             !! Nothing below describes a RUNNING image, because none was observable.\n\
             !! The LAUNCH PATH section is what is on disk, and is still worth reading.\n\n",
            v.get("unreachable_why").and_then(Value::as_str).unwrap_or("no reason given")
        ));
    }
    o.push_str("RUNNING IMAGE — what this process is actually executing\n");
    // `pid` is a plain value in the payload rather than a Fact, because a
    // process reporting on itself always has one — except in the unreachable
    // report, where there is no process at all.
    let pid = fv(v, &["running", "pid"]);
    p(
        &mut o,
        "pid",
        if pid.is_null() {
            "UNDETERMINABLE — no process answered, so there is no pid".to_string()
        } else {
            pid.to_string()
        },
    );
    p(&mut o, "cdhash", show(fv(v, &["running", "code_signature", "cdhash"])));
    p(&mut o, "identifier", show(fv(v, &["running", "code_signature", "identifier"])));
    p(&mut o, "verdict", show(fv(v, &["running", "code_signature", "verdict"])));
    p(&mut o, "properly signed", show(fv(v, &["running", "code_signature", "properly_signed"])));
    p(&mut o, "ad-hoc", show(fv(v, &["running", "code_signature", "ad_hoc"])));
    p(&mut o, "signed for deploy", show(fv(v, &["running", "code_signature", "signed_for_deploy"])));
    p(&mut o, "authority", show(fv(v, &["running", "code_signature", "authority"])));
    p(&mut o, "designated req", show(fv(v, &["running", "code_signature", "designated_requirement"])));
    p(&mut o, "  attributed from", fv(v, &["running", "code_signature", "attribution", "source"])
        .as_str().unwrap_or("— nothing on disk carries this image's code").to_string());
    p(&mut o, "text inode", show(fv(v, &["running", "text_inode"])));
    p(&mut o, "built at", show(fv(v, &["running", "build", "built_at"])));
    p(&mut o, "commit", show(fv(v, &["running", "build", "commit"])));
    p(&mut o, "commit dirty", show(fv(v, &["running", "build", "commit_dirty"])));

    o.push_str("\nLAUNCH PATH — the file at that path RIGHT NOW (path-derived)\n");
    p(&mut o, "path", show(fv(v, &["launch_path", "path"])));
    p(&mut o, "inode", show(fv(v, &["launch_path", "inode"])));
    p(&mut o, "cdhash", show(fv(v, &["launch_path", "code_signature", "cdhash"])));
    p(&mut o, "identifier", show(fv(v, &["launch_path", "code_signature", "identifier"])));
    p(&mut o, "ad-hoc", show(fv(v, &["launch_path", "code_signature", "ad_hoc"])));
    p(&mut o, "signed for deploy", show(fv(v, &["launch_path", "code_signature", "signed_for_deploy"])));

    o.push_str("\nDIVERGENCE\n");
    p(&mut o, "diverged", show(fv(v, &["divergence", "diverged"])));
    p(&mut o, "inodes match", show(fv(v, &["divergence", "by_inode", "same"])));
    p(&mut o, "cdhashes match", show(fv(v, &["divergence", "by_code_identity", "same"])));
    o.push_str(&format!(
        "  {}\n",
        fv(v, &["divergence", "meaning"]).as_str().unwrap_or("")
    ));

    o.push_str("\nABSORBABLE — is something waiting to deploy, and why isn't it\n");
    p(&mut o, "would absorb", show(fv(v, &["absorbable", "would_absorb"])));
    o.push_str(&format!(
        "  {}\n",
        fv(v, &["absorbable", "reason"]).as_str().unwrap_or("")
    ));

    o.push_str("\nBOOT\n");
    p(&mut o, "image booted at", show(fv(v, &["boot", "image_booted_at"])));
    p(&mut o, "process started at", show(fv(v, &["boot", "process_started_at"])));
    p(&mut o, "re-exec'd since start", show(fv(v, &["boot", "re_exec_since_process_start"])));
    o.push_str("  (the process keeps its pid across a re-exec, so `ps` elapsed time is the\n   \
                ORIGINAL launch and NOT the age of the running image)\n");
    p(&mut o, "boot_id (memory only)", show(fv(v, &["boot", "boot_id", "value"])));
    p(&mut o, "boot_identity (on disk)", show(fv(v, &["boot", "boot_identity", "value"])));
    p(&mut o, "  matches running", show(fv(v, &["boot", "boot_identity", "matches_running_image"])));

    o
}

// ──────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undeterminable_fact_never_renders_as_a_value() {
        let f = Fact::unknown("codesign was not on the path");
        let j = f.json();
        assert_eq!(j["known"], false);
        assert_eq!(j["value"], Value::Null);
        assert_eq!(j["why"], "codesign was not on the path");
        // And it must not read as a value in the human rendering either.
        assert!(show(&j).starts_with("UNDETERMINABLE — "), "{}", show(&j));
    }

    #[test]
    fn a_known_fact_carries_its_value() {
        let j = Fact::known(7u64).json();
        assert_eq!(j["known"], true);
        assert_eq!(j["value"], 7);
        assert_eq!(show(&j), "7");
    }

    #[test]
    fn a_garbage_signature_blob_is_unparseable_not_unsigned() {
        // The distinction the whole module turns on: failing to read a
        // signature must never render as "there is no signature".
        let err = parse_superblob(&[0u8; 64]).unwrap_err();
        assert!(err.contains("not an embedded signature superblob"), "{err}");
        assert!(parse_superblob(&[]).is_err());
        // Right magic, truncated index.
        let mut b = vec![0u8; 12];
        b[0..4].copy_from_slice(&CSMAGIC_EMBEDDED_SIGNATURE.to_be_bytes());
        b[8..12].copy_from_slice(&3u32.to_be_bytes());
        assert!(parse_superblob(&b).is_err());
    }
}
