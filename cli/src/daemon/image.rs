//! **Has the image at my launch path changed?** (R6.S6b, H179-1; inspector
//! finding F-I2 on R6.S6.)
//!
//! The holder's self-update used to compare the launch path's **mtime** and
//! nothing else. `promote` (`launchd::place_image`) copies with
//! `std::fs::copy`, which on APFS is a clone that carries the source's mtime to
//! the nanosecond — measured by the R6.S6 inspector. So a promote whose source
//! had the same mtime as the file it replaced (a re-signed copy `touch -r`'d,
//! a `cp -p` rollback copy) put new bytes at the launch path and the holder
//! never noticed.
//!
//! The rule now, cheapest check first:
//!
//! 1. A different **file** (device + inode) at the path is a change. That is
//!    every rename-into-place deploy — `promote`, `install`, a `mv` — whatever
//!    its mtime. (A rename of identical bytes is still a restart onto identical
//!    bytes; that is harmless and keeps "a deploy restarts the holder" true.)
//! 2. The same file with a different **size or mtime** is a change, exactly as
//!    before.
//! 3. The same file, size and mtime but a different **ctime** means something
//!    wrote to it or to its metadata, and userland cannot set ctime, so a
//!    rewrite that put the old mtime back still moves it. That case alone
//!    reads the file and compares its **sha256** with the one taken at start:
//!    different bytes are a change; identical bytes (a `chmod`, an xattr) are
//!    NOT, and the new stat becomes the baseline so the file is not re-read
//!    every pass.
//!
//! Content-hashing every pass would cost a full read of the binary every
//! `poll_secs`; stat-only would either miss an mtime-restoring in-place
//! rewrite (inode + mtime) or restart the holder on a `chmod` (ctime), and a
//! hand-run holder that exits is a holder that is DOWN. So the hash is read
//! once at start (off the supervision thread) and again only when the stat
//! cannot decide.

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

/// What `stat` says about the launch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: (i64, i64),
    pub ctime: (i64, i64),
}

impl Stamp {
    pub fn of(p: &Path) -> std::io::Result<Stamp> {
        let m = std::fs::metadata(p)?;
        Ok(Stamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
    }
}

/// What the stat alone says.
#[derive(Debug, PartialEq, Eq)]
pub enum Judged {
    /// Nothing moved.
    Same,
    /// A different file, or the same file with a new size or mtime.
    Changed(&'static str),
    /// Only the ctime moved: the stat cannot tell a rewrite that restored the
    /// mtime from a metadata-only change. Read the bytes.
    ReadTheBytes,
}

pub fn judge(was: &Stamp, now: &Stamp) -> Judged {
    if (was.dev, was.ino) != (now.dev, now.ino) {
        return Judged::Changed("a different file (inode)");
    }
    if was.size != now.size {
        return Judged::Changed("size");
    }
    if was.mtime != now.mtime {
        return Judged::Changed("mtime");
    }
    if was.ctime != now.ctime {
        return Judged::ReadTheBytes;
    }
    Judged::Same
}

pub fn sha256(p: &Path) -> std::io::Result<[u8; 32]> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// The holder's view of its own launch path, taken at start.
pub struct Watched {
    stamp: Stamp,
    /// The sha256 taken at start, read on a background thread so the first
    /// supervision pass is not delayed by it (a full read of a debug binary
    /// cost ~300 ms, measured, and `supervisor_restarts`'s
    /// `with_auto_restart_off_a_killed_service_stays_down` kills a service
    /// right after the hold line: a later first pass saw it already dead and
    /// launched it). Joined only when a ctime-only change needs it.
    pending: Option<std::thread::JoinHandle<Option<[u8; 32]>>>,
    /// `None` when the bytes at start could not be read, or the file moved
    /// while they were being read; then a ctime-only change is treated as a
    /// change (the safe direction: a restart onto the same bytes, never a
    /// missed deploy).
    hash: Option<[u8; 32]>,
}

impl Watched {
    pub fn at(p: &Path) -> Option<Watched> {
        let stamp = Stamp::of(p).ok()?;
        let path = p.to_path_buf();
        let pending = std::thread::Builder::new()
            .name("image-hash".into())
            .spawn(move || {
                let h = sha256(&path).ok()?;
                // A baseline is only a baseline if the FILE did not move under
                // it. ctime is left out on purpose: a `chmod` in this window
                // must not cost the baseline (measured: without this, a chmod
                // right after the hold line restarted the holder). What that
                // leaves is an mtime-restoring in-place rewrite landing inside
                // the read itself, which is the same race the start stamp has
                // always had between the exec and the first stat.
                let after = Stamp::of(&path).ok()?;
                ((after.dev, after.ino, after.size, after.mtime) == (stamp.dev, stamp.ino, stamp.size, stamp.mtime))
                    .then_some(h)
            })
            .ok();
        Some(Watched { stamp, pending, hash: None })
    }

    fn baseline(&mut self) -> Option<[u8; 32]> {
        if let Some(t) = self.pending.take() {
            self.hash = t.join().ok().flatten();
        }
        self.hash
    }

    /// `Some(why)` when the image at `p` is no longer the one recorded at
    /// start. A metadata-only change is absorbed into the baseline.
    pub fn changed(&mut self, p: &Path) -> Option<String> {
        let now = Stamp::of(p).ok()?;
        match judge(&self.stamp, &now) {
            Judged::Same => None,
            Judged::Changed(why) => Some(why.to_string()),
            Judged::ReadTheBytes => {
                let was = self.baseline();
                let h = sha256(p).ok();
                match (was, h) {
                    (Some(a), Some(b)) if a == b => {
                        self.stamp = now;
                        None
                    }
                    (Some(_), Some(_)) => Some("content (same inode, size and mtime; sha256 differs)".to_string()),
                    _ => Some("ctime, and the bytes could not be compared".to_string()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ino: u64, size: u64, m: i64, c: i64) -> Stamp {
        Stamp { dev: 1, ino, size, mtime: (m, 0), ctime: (c, 0) }
    }

    #[test]
    fn judge_reads_inode_size_mtime_then_defers_ctime_to_the_bytes() {
        let a = s(10, 100, 5, 5);
        assert_eq!(judge(&a, &a), Judged::Same);
        // the F-I2 shape: a new file with the SAME mtime
        assert_eq!(judge(&a, &s(11, 100, 5, 9)), Judged::Changed("a different file (inode)"));
        assert_eq!(judge(&a, &s(10, 101, 5, 9)), Judged::Changed("size"));
        assert_eq!(judge(&a, &s(10, 100, 6, 9)), Judged::Changed("mtime"));
        assert_eq!(judge(&a, &s(10, 100, 5, 9)), Judged::ReadTheBytes);
    }

    #[test]
    fn a_ctime_only_change_is_a_change_only_when_the_bytes_differ() {
        let dir = std::env::temp_dir().join(format!("cauldron-image-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("img");
        std::fs::write(&p, b"0123456789").unwrap();
        let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
        let mut w = Watched::at(&p).unwrap();
        assert_eq!(w.changed(&p), None);

        // metadata only: chmod moves ctime, bytes identical -> NOT a change
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let after_chmod = Stamp::of(&p).unwrap();
        assert_eq!(judge(&w.stamp, &after_chmod), Judged::ReadTheBytes, "chmod did not move ctime; the case below is not reached");
        assert_eq!(w.changed(&p), None, "a chmod restarted the holder");
        assert_eq!(w.changed(&p), None);

        // same inode, same size, mtime put back, different bytes -> a change
        let before = Stamp::of(&p).unwrap();
        {
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            use std::os::unix::fs::FileExt;
            f.write_at(b"X", 9).unwrap();
            f.set_modified(mtime).unwrap();
        }
        let after = Stamp::of(&p).unwrap();
        assert_eq!((before.ino, before.size, before.mtime), (after.ino, after.size, after.mtime), "the rewrite moved inode/size/mtime; the case is not reached");
        assert!(w.changed(&p).unwrap().starts_with("content"), "an mtime-restoring in-place rewrite was not seen");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
