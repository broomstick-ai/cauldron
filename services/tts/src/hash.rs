//! The identity of a render — and nothing else.
//!
//! ```text
//! hash = sha256_hex( lowercase(collapse_ws(trim(text))) + "|" + fingerprint )
//! ```
//!
//! Byte for byte the face's own cache key, with the module fingerprint where
//! that key has its voice. The `system` module's fingerprint IS the voice
//! (`"Samantha"`), so every file the operator's cache already holds is named
//! by this function and adopts by filename alone. It must not drift: a change
//! here orphans every cached byte.
//!
//! The text itself is never stored. Registration is the one moment text
//! crosses the wire, because a hash cannot be synthesized; after that the
//! service deals only in hashes.

use sha2::{Digest, Sha256};

/// Trim, collapse runs of whitespace to one space, lowercase.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

pub fn hash_for(text: &str, fingerprint: &str) -> String {
    let digest = Sha256::digest(format!("{}|{}", normalize(text), fingerprint).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 64 lowercase hex characters. Nothing else is ever accepted as a hash off
/// the wire, because a hash becomes a path.
pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The audio file's path under `audio/`, sharded 256 x 256 on the hash so no
/// directory holds more than a few files. Derived, never stored.
pub fn rel_path(hash: &str, ext: &str) -> String {
    format!("{}/{}/{hash}.{ext}", &hash[0..2], &hash[2..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against the face's key by recomputing it independently. If this
    /// fails, every byte of the operator's cache is orphaned.
    #[test]
    fn the_hash_is_the_faces_cache_key() {
        let manual: String =
            Sha256::digest(b"hello there|Samantha").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash_for("  Hello \n  There  ", "Samantha"), manual);
        assert!(is_hash(&manual));
        assert!(!is_hash(&manual.to_uppercase()));
        assert!(!is_hash(&manual[1..]));
    }

    #[test]
    fn normalizing_keeps_punctuation() {
        assert_eq!(normalize("  A\n\tB  C "), "a b c");
        assert_eq!(normalize("Don't — stop"), "don't — stop");
    }

    #[test]
    fn the_shard_is_read_off_the_hash() {
        let h = hash_for("x", "Samantha");
        assert_eq!(rel_path(&h, "aiff"), format!("{}/{}/{h}.aiff", &h[0..2], &h[2..4]));
    }
}
