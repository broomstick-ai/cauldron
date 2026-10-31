//! A build label.
//!
//! A version is a *named build*, and the name is a fingerprint of the bytes
//! that were built. This is FNV-1a, 64 bits, rendered as 16 hex characters —
//! a content fingerprint, deliberately not called a digest and not used for
//! anything that needs one. It distinguishes builds, which is all a version
//! label has to do, and it costs no dependency.

pub fn fingerprint(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn fingerprint_file(path: &std::path::Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("reading {} to fingerprint it: {e}", path.display()))?;
    Ok(fingerprint(&bytes))
}
