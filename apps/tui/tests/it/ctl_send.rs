//! **RV1.S12a: `--send` AND THE SEND KEY ARE ONE FUNCTION.**
//!
//! `cauldron ctl dump` is `tui --cauldron --dump --send` with the bucket flags
//! (`cli/src/ctl.rs`), so the coordinator's dump is the operator's dump only
//! if `--send` sends through what Enter in the bucket calls. This proves it on
//! one environment, with the two writers DIFFERENT THINGS:
//!
//! - the KEY: an `App` opened writing (as his face is), the bucket built by
//!   `cli::apply_dump_ops` (the flags' own function), and Enter pressed
//!   through `App::handle` with the bucket focused;
//! - the FLAG: the built binary, launched through [`crate::face::Face`], with
//!   the same flags plus `--send`.
//!
//! Both lines land in the environment's `dumps.jsonl`; parsed, with `seq` and
//! `at` masked, they are equal, and the line the flag PRINTED is the line it
//! wrote, byte for byte. Opposed: a third send whose flags carry one extra
//! `--quote` differs from the key's line, so the comparison can fail.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tui::cauldron::app::App;
use tui::cauldron::cli::{apply_dump_ops, DumpOp};

use crate::face::Face;

const ENV: &str = "drift";

fn scratch() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rv1s12a-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: name.into(), name_source: Source::Default, data_root: root.join("data").join(name) }
}

/// A scratch dir whose `cauldron/` is the root [`Face`] hands the binary
/// (`CAULDRON_HOME`), holding the environments named.
fn root_with(envs: &[&str]) -> (PathBuf, PathBuf) {
    let s = scratch();
    let root = s.join("cauldron");
    let here = env_in(&root, "prod");
    here.ensure_dirs().unwrap();
    for e in envs {
        tui::cauldron::store::create_env(&here, e).unwrap();
    }
    (s, root)
}

fn log(root: &Path, env: &str) -> PathBuf {
    tui::cauldron::store::soup_dir(&env_in(root, env)).unwrap().join("dumps.jsonl")
}

fn lines(root: &Path, env: &str) -> Vec<String> {
    std::fs::read_to_string(log(root, env)).unwrap_or_default().lines().map(str::to_string).collect()
}

/// The bucket both writers build: a draft, a second note, and a quote.
fn ops() -> Vec<DumpOp> {
    vec![
        DumpOp::Draft("the coordinator looked at the ledger".into()),
        DumpOp::Note("a second note, under a blank line".into()),
        DumpOp::Quote("Q-1".into()),
    ]
}

fn flags(ops: &[DumpOp]) -> Vec<String> {
    let mut v = Vec::new();
    for op in ops {
        let (f, t) = match op {
            DumpOp::Draft(t) => ("--draft", t),
            DumpOp::Note(t) => ("--note", t),
            DumpOp::Quote(t) => ("--quote", t),
            DumpOp::Paste(t) => ("--paste", t),
        };
        v.push(f.to_string());
        v.push(t.clone());
    }
    v
}

/// The record with what differs between two sends by construction removed.
fn masked(line: &str) -> Value {
    let mut v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("not a dump record ({e}): {line}"));
    let o = v.as_object_mut().expect("a dump record is an object");
    assert!(o.remove("seq").is_some() && o.remove("at").is_some(), "the record lost seq/at: {line}");
    v
}

/// Run the built face on `root` with `args`: (rc, stdout, stderr).
fn face(s: &Path, args: &[String]) -> (i32, String, String) {
    let o = Face::new(s).args(args).output().expect("run the face");
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn send_args(env: &str, extra: &[String]) -> Vec<String> {
    let mut a: Vec<String> = ["--cauldron", "--env", env, "--dump", "--send"].iter().map(|s| s.to_string()).collect();
    a.extend(extra.iter().cloned());
    a
}

/// **C1 at the face: the flag's dump IS the key's dump.**
#[test]
fn rv1s12a_send_flag_and_send_key_append_the_same_dump() {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let (s, root) = root_with(&[ENV]);
    assert!(lines(&root, ENV).is_empty(), "a fresh environment has no dumps");

    // THE KEY: his face, writing; the bucket built by the flags' function;
    // Enter in the bucket.
    let env = env_in(&root, ENV);
    let mut a = App::open(env.clone(), tui::cauldron::store::no_seeds(&env), true).expect("opens writing");
    apply_dump_ops(&mut a, &ops()).unwrap();
    a.focus_input();
    a.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(a.error.is_none(), "the key's send failed: {:?}", a.error);
    drop(a);
    let after_key = lines(&root, ENV);
    assert_eq!(after_key.len(), 1, "Enter appended exactly one dump: {after_key:?}");

    // THE FLAG: the built binary, the same flags, plus --send.
    let (rc, out, err) = face(&s, &send_args(ENV, &flags(&ops())));
    assert_eq!(rc, 0, "--send failed: {err}");
    let after_flag = lines(&root, ENV);
    assert_eq!(after_flag.len(), 2, "--send appended exactly one dump: {after_flag:?}");
    assert_eq!(out.trim_end_matches('\n'), after_flag[1], "what --send printed is the line it wrote, byte for byte");
    assert_ne!(after_key[0], after_flag[1], "sentinel: two sends are two lines (seq/at differ)");
    assert_eq!(masked(&after_key[0]), masked(&after_flag[1]), "the key and --send wrote different dumps");

    // OPPOSED: one extra --quote on the flag's side is a different dump.
    let mut more = ops();
    more.push(DumpOp::Quote("Q-2".into()));
    let (rc, _, err) = face(&s, &send_args(ENV, &flags(&more)));
    assert_eq!(rc, 0, "{err}");
    let third = lines(&root, ENV);
    assert_eq!(third.len(), 3);
    assert_ne!(masked(&after_key[0]), masked(&third[2]), "the comparison cannot tell an extra quote apart");
    let _ = std::fs::remove_dir_all(&s);
}

/// **`--send` is valid only with `--dump`, writes nothing when refused, and
/// is refused on the demo and on an empty bucket.** Opposed by the first
/// test: the same flags WITH `--dump` append one line.
#[test]
fn rv1s12a_send_is_refused_without_dump_on_the_demo_and_on_an_empty_bucket() {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let (s, root) = root_with(&[ENV, soup::demo::ENV]);
    let size = |e: &str| std::fs::metadata(log(&root, e)).map(|m| m.len()).unwrap_or(0);

    // Without --dump (no terminal: the run is scripted all the same).
    let mut no_dump: Vec<String> = ["--cauldron", "--env", ENV, "--send"].iter().map(|s| s.to_string()).collect();
    no_dump.extend(flags(&ops()));
    let (rc, _, err) = face(&s, &no_dump);
    assert_ne!(rc, 0, "--send without --dump was accepted");
    assert!(err.contains("only with --dump"), "{err}");
    assert_eq!(size(ENV), 0, "a refused --send wrote a dump");

    // The demo is read-only.
    let (rc, _, err) = face(&s, &send_args(soup::demo::ENV, &flags(&ops())));
    assert_ne!(rc, 0, "--send into the demo was accepted");
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(size(soup::demo::ENV), 0, "a dump was appended to the demo");

    // An empty bucket.
    let (rc, _, err) = face(&s, &send_args(ENV, &[]));
    assert_ne!(rc, 0, "an empty --send was accepted");
    assert!(err.contains("bucket is empty"), "{err}");
    assert_eq!(size(ENV), 0, "an empty --send wrote a dump");

    // OPPOSED: the same environment and flags, valid, append exactly one.
    let (rc, _, err) = face(&s, &send_args(ENV, &flags(&ops())));
    assert_eq!(rc, 0, "{err}");
    assert_eq!(lines(&root, ENV).len(), 1);
    let _ = std::fs::remove_dir_all(&s);
}

// ── RV1.S12c: his draft never rides out in a ctl dump ──────────────────────

const HIS: &str = "OPERATOR PRIVATE DRAFT";

/// His face: the environment's real bucket, writing — the store his face
/// keeps (`App::draft` opened with `persist`).
fn his_bucket(root: &Path, env: &str) -> tui::cauldron::store::DraftStore {
    let dir = tui::cauldron::store::soup_dir(&env_in(root, env)).unwrap();
    let mut d = tui::cauldron::store::DraftStore::new(&dir, true);
    d.load().unwrap();
    d
}

/// One write from his face: `text` saved as he would type it.
fn he_types(d: &mut tui::cauldron::store::DraftStore, text: &str) {
    d.save(text, text.len()).expect("his face saves its bucket");
}

fn bucket_bytes(root: &Path, env: &str) -> Vec<u8> {
    let p = tui::cauldron::store::soup_dir(&env_in(root, env)).unwrap().join("bucket.json");
    std::fs::read(&p).unwrap_or_else(|e| panic!("his bucket {}: {e}", p.display()))
}

/// What ctl's ops build with NO draft anywhere: a second environment with
/// no `bucket.json` at all, opened as `--dump --send` opens it.
fn ctl_text_alone(root: &Path) -> String {
    let env = env_in(root, "alone");
    tui::cauldron::store::create_env(&env_in(root, "prod"), "alone").unwrap();
    let mut a = App::open(env.clone(), tui::cauldron::store::no_seeds(&env), false).expect("opens as --dump");
    tui::cauldron::cli::arm_send(&mut a).unwrap();
    apply_dump_ops(&mut a, &ops()).unwrap();
    a.composer.text()
}

/// **RV1.S12c: A FACE WRITE MID-SEND DOES NOT REACH THE DUMP.** Two writers
/// on one environment, interleaved at every point the race could land: his
/// face (the real `bucket.json`, writing) and ctl's `--send` path (the App
/// opened as `--dump` opens it, `arm_send`, the bucket flags' own function,
/// `send_bucket`). He writes after ctl opens his environment, after it arms,
/// and after every op it applies. The dump carries ctl's text and nothing
/// of his; his bucket is byte-for-byte what his last write left.
///
/// Sentinels: each of his writes really changed the file (the bytes differ
/// write to write — two different things), and a face that opens the
/// environment afterwards reads HIS last text from that file, so the file
/// the writes went to is the one his face reads.
#[test]
fn rv1s12c_a_face_write_mid_send_never_reaches_the_dump_and_his_draft_is_untouched() {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let (s, root) = root_with(&[ENV]);
    let want = ctl_text_alone(&root);
    assert!(!want.is_empty() && !want.contains(HIS), "ctl's own text: {want:?}");

    let mut his = his_bucket(&root, ENV);
    let mut n = 0;
    let mut wrote = Vec::<Vec<u8>>::new();
    let mut said = String::new();
    let mut write = |his: &mut tui::cauldron::store::DraftStore| {
        n += 1;
        said = format!("{HIS} — line {n} of what he is writing");
        he_types(his, &said);
        wrote.push(bucket_bytes(&root, ENV));
        said.clone()
    };
    write(&mut his);

    // ctl opens his environment as `--dump --send` does (App::open loads his
    // bucket), then he writes; ctl arms, then he writes.
    let env = env_in(&root, ENV);
    let mut ctl = App::open(env.clone(), tui::cauldron::store::no_seeds(&env), false).expect("opens as --dump");
    write(&mut his);
    tui::cauldron::cli::arm_send(&mut ctl).unwrap();
    write(&mut his);
    // Every op ctl applies saves its draft; he writes after each.
    for op in ops() {
        apply_dump_ops(&mut ctl, std::slice::from_ref(&op)).unwrap();
        write(&mut his);
    }
    let last = write(&mut his);
    let left = bucket_bytes(&root, ENV);
    drop(write);
    assert!(wrote.windows(2).all(|w| w[0] != w[1]), "sentinel: each of his writes changed the file");
    assert_eq!(wrote.len(), 2 + 1 + ops().len() + 1);

    let before = lines(&root, ENV).len();
    let sent = tui::cauldron::cli::send_bucket(&mut ctl).expect("ctl sends");
    assert_eq!(lines(&root, ENV).len(), before + 1, "exactly one dump appended");
    assert!(!sent.text.contains(HIS), "his draft rode out in ctl's dump: {:?}", sent.text);
    assert_eq!(sent.text, want, "the dump is not ctl's text alone");
    drop(ctl);
    assert_eq!(bucket_bytes(&root, ENV), left, "his bucket changed under a ctl send");

    // Sentinel: the file his writes went to is the one his face reads.
    let face = App::open(env.clone(), tui::cauldron::store::no_seeds(&env), false).expect("his face opens");
    assert_eq!(face.composer.text(), last, "his face does not read the bucket the writes went to");
    let _ = std::fs::remove_dir_all(&s);
}

/// **RV1.S12c, the built binary: `--send` runs while his face writes.** A
/// thread is his face, writing his bucket without pause; meanwhile the built
/// face runs `--dump --send` three times. Every dump printed is ctl's text
/// alone, and his bucket ends as his own last write left it.
#[test]
fn rv1s12c_send_while_his_face_writes_without_pause() {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let (s, root) = root_with(&[ENV]);
    let want = ctl_text_alone(&root);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (root, stop) = (root.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut his = his_bucket(&root, ENV);
            let mut n = 0usize;
            let mut last = String::new();
            while !stop.load(Ordering::SeqCst) || n < 2 {
                n += 1;
                last = format!("{HIS} — keystroke {n}");
                he_types(&mut his, &last);
            }
            (n, last)
        })
    };
    for _ in 0..3 {
        let (rc, out, err) = face(&s, &send_args(ENV, &flags(&ops())));
        assert_eq!(rc, 0, "--send failed: {err}");
        let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
        let text = v["text"].as_str().unwrap_or_default();
        assert!(!text.contains(HIS), "his draft rode out in the dump: {text:?}");
        assert_eq!(text, want, "the dump is not ctl's text alone");
    }
    stop.store(true, Ordering::SeqCst);
    let (n, last) = writer.join().unwrap();
    assert!(n >= 2, "sentinel: his face wrote while the sends ran ({n} writes)");
    let mut check = tui::cauldron::store::DraftStore::new(&tui::cauldron::store::soup_dir(&env_in(&root, ENV)).unwrap(), false);
    assert_eq!(check.load().unwrap(), last, "his bucket is not his last write");
    assert_eq!(lines(&root, ENV).len(), 3);
    let _ = std::fs::remove_dir_all(&s);
}
