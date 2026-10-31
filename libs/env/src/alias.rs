//! **An environment's data root must BE its own directory (R6.S3).**
//!
//! [`crate::guard`] refuses paths by name and by containment, and
//! [`crate::paths`] computes `data_root = <root>/data/<name>` — but neither
//! asks what that path RESOLVES to. A symlink `data/dev -> data/prod` (or to
//! another root's `data/prod`) passes both: the name is legal, the computed
//! path is "inside" `data/`, and every write `dev` makes lands in production's
//! store. The seam table's own check would refuse the TABLE (its rows say
//! `prod`), but the services' data directories carry no such restatement, so a
//! freshly started `dev` daemon would open prod's sqlite through the link.
//!
//! The rule is structural, not a list: **the canonical data root must equal
//! the canonical `data/` directory joined with the environment's own name.**
//! Anything else — a symlinked environment directory, a symlinked component
//! that lands it elsewhere — is refused at resolution, which is the one moment
//! both halves are known and before any verb reads or writes through it.
//!
//! A data root that does not exist yet cannot alias anything and is accepted;
//! `create_dir_all` makes a real directory, never a link.

use std::path::Path;

use anyhow::{bail, Result};

/// The name whose data root every other environment is kept out of. Named in
/// the refusal when the alias lands on it, because that is the case that costs
/// the operator his data.
pub const PROD: &str = "prod";

/// Refuse a data root that resolves somewhere other than `<root>/data/<name>`.
pub fn refuse_aliased_data_root(root: &Path, name: &str, data_root: &Path) -> Result<()> {
    // `symlink_metadata`, not `exists`: a DANGLING link is still a link, and
    // `exists` follows it and says no — which would let `create_dir_all`
    // later create the link's target instead of a directory of our own.
    match std::fs::symlink_metadata(data_root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => bail!("REFUSED: cannot inspect the data root {}: {e}", data_root.display()),
        Ok(_) => {}
    }
    let data = root.join("data");
    let real = data_root.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "REFUSED: environment `{name}`'s data root {} cannot be resolved ({e}). A data \
             root that is a dangling link is a data root somebody else chose; remove it and \
             let cauldron create the directory.",
            data_root.display()
        )
    })?;
    let expected = data
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("REFUSED: cannot resolve {}: {e}", data.display()))?
        .join(name);
    if real == expected {
        return Ok(());
    }
    let prod_real = data.join(PROD).canonicalize().ok();
    let lands_on_prod = name != PROD && prod_real.as_deref() == Some(real.as_path());
    bail!(
        "REFUSED: environment `{name}`'s data root {} RESOLVES TO {}, which is not its own \
         directory ({}).{} An environment's data root is the environment: every service it \
         hosts would read and write through the link. Remove the link (`ls -l {}` shows it) \
         and seed a real copy with `cauldron env create <name> --from <env>` instead.",
        data_root.display(),
        real.display(),
        expected.display(),
        if lands_on_prod {
            " That is PRODUCTION's data root: running this environment would run on prod's \
             store."
        } else {
            ""
        },
        data_root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "env-alias-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("data/prod")).unwrap();
        p
    }

    /// Opposed pair: a real directory and a not-yet-created one are ACCEPTED
    /// (a constant refusal fails), a link onto prod and a link onto a sibling
    /// are REFUSED (a constant acceptance fails), and prod's own refusal names
    /// production.
    #[test]
    fn a_data_root_that_resolves_elsewhere_is_refused_and_a_real_one_is_not() {
        let root = tmp();
        let data = root.join("data");
        // accepted: real dir, absent dir, prod itself
        std::fs::create_dir_all(data.join("dev")).unwrap();
        refuse_aliased_data_root(&root, "dev", &data.join("dev")).unwrap();
        refuse_aliased_data_root(&root, "fresh", &data.join("fresh")).unwrap();
        refuse_aliased_data_root(&root, "prod", &data.join("prod")).unwrap();
        // refused: a link onto prod
        std::os::unix::fs::symlink(data.join("prod"), data.join("evil")).unwrap();
        let e = refuse_aliased_data_root(&root, "evil", &data.join("evil")).unwrap_err().to_string();
        assert!(e.contains("REFUSED") && e.contains("PRODUCTION"), "{e}");
        // refused: a link onto a non-prod sibling (not named as prod)
        std::os::unix::fs::symlink(data.join("dev"), data.join("twin")).unwrap();
        let e = refuse_aliased_data_root(&root, "twin", &data.join("twin")).unwrap_err().to_string();
        assert!(e.contains("REFUSED") && !e.contains("PRODUCTION"), "{e}");
        // refused: a dangling link
        std::os::unix::fs::symlink(root.join("nowhere"), data.join("dangle")).unwrap();
        assert!(refuse_aliased_data_root(&root, "dangle", &data.join("dangle")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
