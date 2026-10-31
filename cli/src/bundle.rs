//! **A BUNDLE ARTIFACT — a `.app` installed whole, signature seal intact.**
//! (R5.S3)
//!
//! `ui-server` (AUI) runs from `~/Applications/AUI.app/Contents/MacOS/ui-server`
//! because its macOS identity is the BUNDLE's: a bound Info.plist and sealed
//! resources, so a TCC grant survives a rebuild (see harness
//! `~/.aui/supervise-ui-server.sh`, "WHICH BINARY IT RUNS"). The artifact path
//! used to copy ONE FILE into the version directory, which tears the
//! executable out of the bundle and demotes its identity to a path — the
//! exact failure that costs the operator his microphone grants.
//!
//! So an `artifact` that is a DIRECTORY is copied whole with `ditto` (which
//! preserves the code signature, extended attributes and resource forks), and
//! `bin` names the executable inside it (`AUI.app/Contents/MacOS/ui-server`).
//! The designated requirement is identifier + certificate, never a path, so a
//! copied bundle is the same identity — and that is CHECKED, not assumed: when
//! the source bundle verifies under `codesign --verify --strict`, the copy
//! must too, or the install is refused and nothing is left behind.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// Does `codesign --verify --strict` accept this bundle?
pub(crate) fn verifies(bundle: &Path) -> bool {
    Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(bundle)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Copy the bundle at `src` into the version directory `dir` as
/// `dir/<bundle name>`, atomically (copy to a temporary name, then rename).
/// Returns the installed executable, `dir/<bin>`.
pub(crate) fn install(src: &Path, dir: &Path, bin: &str) -> Result<PathBuf> {
    let name = src
        .file_name()
        .with_context(|| format!("the bundle artifact {} has no name", src.display()))?;
    let dest = dir.join(name);
    let tmp = dir.join(format!(".{}.installing.{}", name.to_string_lossy(), std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).with_context(|| format!("clearing {}", tmp.display()))?;
    }
    let st = Command::new("/usr/bin/ditto")
        .arg(src)
        .arg(&tmp)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("running ditto {} {}", src.display(), tmp.display()))?;
    if !st.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("ditto {} -> {} failed ({st})", src.display(), tmp.display());
    }
    // THE SEAL MUST SURVIVE THE COPY. A source that verifies and a copy that
    // does not is a copy that changed the identity, which is the one thing
    // this function exists not to do.
    if verifies(src) && !verifies(&tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!(
            "REFUSED to install the bundle {}: it verifies under `codesign --verify --strict` \
             and the copy does not. Installing it would change the identity macOS grants \
             permissions to.",
            src.display()
        );
    }
    let exe_in_tmp = tmp.join(env::hosting::bin_inside_bundle(bin));
    if !exe_in_tmp.is_file() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!(
            "the bundle {} has no executable at `{}` (from `[build] bin = \"{bin}\"`)",
            src.display(),
            env::hosting::bin_inside_bundle(bin)
        );
    }
    if dest.exists() {
        std::fs::remove_dir_all(&dest).with_context(|| format!("replacing {}", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("renaming {} into {}", tmp.display(), dest.display()))?;
    Ok(dir.join(bin))
}
