//! # The run token — signed claims saying WHICH THREAD the caller is
//!
//! `docs/managed-subagents.md` §5.1: *"It injects `THREADS_RUN_TOKEN`, an
//! HMAC-signed `{thread_id, run_id, root, depth, turn}` … Any other producer
//! holding the machine key can verify the same token without asking threads."*
//! And §7: *"the MECHANISM stays shared, because it costs nothing — a signed
//! claim any producer with the machine key can verify, on a door the chassis
//! already owns."*
//!
//! So the token is the chassis's, exactly as the bearer is: the FORMAT, the
//! mint and the verify live here, and a producer contributes two things the
//! chassis cannot know — the key's bytes and the validity window. `threads`'
//! window is a `running_turns` row (*"a token is void the moment its turn
//! ends"*), checked by the producer AFTER this module has said the signature is
//! genuine; nothing here knows what a turn is.
//!
//! ## What a token proves, and what it does not
//!
//! A verified token proves one thing: **whoever holds the key signed these
//! claims.** The daemon is the only holder, and it signs a token once per turn
//! into the child's environment, so a verified token means *the daemon started
//! a turn on this thread and handed it this identity*. It is not a bearer in
//! [`crate::server::Bearers`]' sense — a bearer proves *you completed the
//! handshake on this socket*; this proves *you are thread X* — and it is not
//! an ACL: what thread X may do is the façade's question (`threads::agents`),
//! not this module's.
//!
//! ## The shape on the wire
//!
//! ```text
//! rt1.<hex(claims as JSON)>.<hex(HMAC-SHA256(key, "rt1." + hex(claims)))>
//! ```
//!
//! Hex rather than base64, deliberately: the workspace has no base64 crate and
//! a hand-rolled one is thirty lines of alphabet arithmetic that a reviewer has
//! to check character by character; hex is `{:02x}` and a table. The cost is
//! that a token is ~2× longer than it could be — a few hundred bytes in an
//! environment variable, which nothing pays for twice.
//!
//! The MAC is over the prefix AND the payload, so a token minted under a future
//! `rt2` cannot be replayed as `rt1` with the same bytes. HMAC is the textbook
//! construction over the `sha2` the resource door already links —
//! `H((K ⊕ opad) ‖ H((K ⊕ ipad) ‖ m))` — written out here rather than taken from
//! an `hmac` crate for the reason `check-features.sh` states about every
//! dependency behind `server`: a mirror build must link none of it, and one
//! more crate is one more thing that script has to assert the floor does not
//! pull.
//!
//! ## Feature placement
//!
//! [`Claims`] and the hex helpers are in the always-compiled floor: a consumer
//! that is handed a token may want to READ its claims (they are not secret —
//! the signature is what matters, and the claims are plain hex). [`Key`],
//! `mint` and `verify` are behind `server`, because only a producer signs or
//! checks, and because `sha2` is behind `server`.

use serde::{Deserialize, Serialize};

/// The token format's version tag. The MAC covers it.
pub const PREFIX: &str = "rt1";

/// **WHO THE CALLER IS**, as the daemon that spawned it says.
///
/// Exactly the five fields `docs/managed-subagents.md` §5.1 names. None is
/// optional and none is defaulted: a token missing a field is malformed, not
/// a token with a blank in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    /// The thread the caller IS. The façade binds this into every act as the
    /// caller's identity; the caller cannot name a different one.
    pub thread_id: String,
    /// The run (episode) this turn belongs to — `agent_runs.(thread_id, seq)`
    /// once R4.S2 lands; until then whatever the minter put here.
    pub run_id: String,
    /// The root thread of the tree this caller is in.
    pub root: String,
    /// Depth below the root. `0` is a root.
    pub depth: u32,
    /// **THE VALIDITY WINDOW'S HANDLE.** `threads` writes the turn's spawn
    /// cookie here, and verifies a token only while the `running_turns` row
    /// for `thread_id` carries the same cookie — which is what makes *"void the
    /// moment its turn ends"* a check rather than a hope.
    pub turn: String,
}

/// Why a token was refused. Spelled as an enum rather than prose so a producer
/// can map each to a code; `Display` is what reaches the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Not three dot-separated parts, wrong prefix, or a part that is not hex.
    Malformed(&'static str),
    /// The MAC does not match: a different key, or tampered claims.
    BadSignature,
    /// The signature is genuine and the claims do not parse. This is the
    /// minter's defect, not the presenter's.
    BadClaims(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Malformed(why) => write!(f, "malformed run token: {why}"),
            TokenError::BadSignature => f.write_str(
                "bad signature: the token was not minted by this daemon's key, or its claims \
                 were altered after minting",
            ),
            TokenError::BadClaims(e) => write!(f, "the token's claims do not parse: {e}"),
        }
    }
}

impl std::error::Error for TokenError {}

/// Lowercase hex, two characters per byte.
pub fn hex_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(TABLE[(b >> 4) as usize] as char);
        s.push(TABLE[(b & 0x0f) as usize] as char);
    }
    s
}

/// The inverse of [`hex_encode`]. `None` on an odd length or a non-hex byte;
/// uppercase is accepted.
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks(2) {
        out.push((nib(pair[0])? << 4) | nib(pair[1])?);
    }
    Some(out)
}

/// Split a token into `(claims hex, mac hex)` after checking its shape. Shared
/// by the floor's claim reader and the signed verifier so the two cannot
/// disagree about what a token looks like.
fn split(token: &str) -> Result<(&str, &str), TokenError> {
    let mut parts = token.split('.');
    let (Some(prefix), Some(claims), Some(mac), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed(
            "expected three dot-separated parts: prefix.claims.mac",
        ));
    };
    if prefix != PREFIX {
        return Err(TokenError::Malformed("unknown prefix; this daemon mints `rt1`"));
    }
    if claims.is_empty() || mac.is_empty() {
        return Err(TokenError::Malformed("empty claims or mac"));
    }
    Ok((claims, mac))
}

/// **READ THE CLAIMS WITHOUT VERIFYING THEM.** For a consumer that wants to
/// know who a token says it is — a log line, a face — and has no key. The
/// result is UNTRUSTED by construction and the name says so; a producer deciding
/// anything on it has skipped the signature.
pub fn peek_unverified(token: &str) -> Result<Claims, TokenError> {
    let (claims_hex, _) = split(token)?;
    let bytes = hex_decode(claims_hex)
        .ok_or(TokenError::Malformed("the claims part is not hex"))?;
    serde_json::from_slice(&bytes).map_err(|e| TokenError::BadClaims(e.to_string()))
}

#[cfg(feature = "server")]
mod signed {
    use super::{hex_decode, hex_encode, split, Claims, TokenError, PREFIX};
    use sha2::{Digest, Sha256};
    use std::path::Path;

    /// **THE MACHINE KEY.** 32 bytes, held by the producer, never on the wire.
    ///
    /// `Debug` is redacted for the reason [`crate::envelope::Bearer`]'s is: the
    /// way a secret normally escapes is a `{:?}` on a struct that happens to
    /// hold it. There is no `Display` at all.
    #[derive(Clone, PartialEq, Eq)]
    pub struct Key([u8; Key::LEN]);

    impl std::fmt::Debug for Key {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Key(<redacted>)")
        }
    }

    impl Key {
        /// The key length. HMAC-SHA256's block size is 64; a 32-byte key is
        /// zero-padded to it, which is the construction's own rule for a key
        /// shorter than a block.
        pub const LEN: usize = 32;

        pub fn from_bytes(bytes: [u8; Key::LEN]) -> Self {
            Self(bytes)
        }

        /// **READ THE KEY FROM `path`, OR MINT ONE THERE.**
        ///
        /// The daemon calls this once at construction against
        /// `<data_dir>/run-token.key`. A file of exactly [`Key::LEN`] bytes is
        /// the key; a missing file is minted from two v4 UUIDs (122 random bits
        /// each) and written `0600` with `create_new`, so two daemons racing
        /// for the same directory cannot both mint — the loser re-reads what
        /// the winner wrote. A file of the wrong length is an error, not a
        /// re-mint: silently replacing a key would void every token minted
        /// under the old one, and nothing would say so.
        pub fn load_or_mint(path: &Path) -> std::io::Result<Self> {
            use std::io::{Error, ErrorKind};
            match std::fs::read(path) {
                Ok(bytes) => Self::from_file_bytes(path, bytes),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    let mut bytes = [0u8; Key::LEN];
                    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                    let mut opts = std::fs::OpenOptions::new();
                    opts.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        opts.mode(0o600);
                    }
                    match opts.open(path) {
                        Ok(mut f) => {
                            use std::io::Write;
                            f.write_all(&bytes)?;
                            f.sync_all()?;
                            Ok(Self(bytes))
                        }
                        // Lost the race to another minter: theirs is the key.
                        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                            Self::from_file_bytes(path, std::fs::read(path)?)
                        }
                        Err(e) => Err(Error::new(
                            e.kind(),
                            format!("minting the run-token key at {}: {e}", path.display()),
                        )),
                    }
                }
                Err(e) => Err(Error::new(
                    e.kind(),
                    format!("reading the run-token key at {}: {e}", path.display()),
                )),
            }
        }

        fn from_file_bytes(path: &Path, bytes: Vec<u8>) -> std::io::Result<Self> {
            let arr: [u8; Key::LEN] = bytes.as_slice().try_into().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{} holds {} bytes; a run-token key is exactly {} — refusing to \
                         guess, because re-minting would silently void every token minted \
                         under the old key",
                        path.display(),
                        bytes.len(),
                        Key::LEN
                    ),
                )
            })?;
            Ok(Self(arr))
        }

        /// **SIGN THESE CLAIMS.** The only way a token comes into existence.
        pub fn mint(&self, claims: &Claims) -> String {
            let payload = hex_encode(&serde_json::to_vec(claims).expect("Claims is five scalars"));
            let signed = format!("{PREFIX}.{payload}");
            let mac = hmac_sha256(&self.0, signed.as_bytes());
            format!("{signed}.{}", hex_encode(&mac))
        }

        /// **CHECK A TOKEN AND HAND BACK ITS CLAIMS.**
        ///
        /// The order is: shape, then signature, then claims. The signature is
        /// checked BEFORE the claims are parsed, so a presenter cannot learn
        /// anything about how claims are read by sending unsigned ones — and the
        /// compare is constant-time, for the reason the daemon secret's is: a
        /// MAC is a value an attacker can iterate on, and a byte-by-byte compare
        /// leaks which byte was wrong.
        pub fn verify(&self, token: &str) -> Result<Claims, TokenError> {
            let (claims_hex, mac_hex) = split(token)?;
            let presented = hex_decode(mac_hex)
                .ok_or(TokenError::Malformed("the mac part is not hex"))?;
            let signed = format!("{PREFIX}.{claims_hex}");
            let expected = hmac_sha256(&self.0, signed.as_bytes());
            if !constant_time_eq(&expected, &presented) {
                return Err(TokenError::BadSignature);
            }
            let bytes = hex_decode(claims_hex)
                .ok_or(TokenError::Malformed("the claims part is not hex"))?;
            serde_json::from_slice(&bytes).map_err(|e| TokenError::BadClaims(e.to_string()))
        }
    }

    /// HMAC-SHA256, RFC 2104, block size 64.
    fn hmac_sha256(key: &[u8; Key::LEN], msg: &[u8]) -> [u8; 32] {
        const BLOCK: usize = 64;
        let mut k = [0u8; BLOCK];
        k[..Key::LEN].copy_from_slice(key);
        let mut ipad = [0u8; BLOCK];
        let mut opad = [0u8; BLOCK];
        for i in 0..BLOCK {
            ipad[i] = k[i] ^ 0x36;
            opad[i] = k[i] ^ 0x5c;
        }
        let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
        let outer = Sha256::new().chain_update(opad).chain_update(inner).finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&outer);
        out
    }

    fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn claims() -> Claims {
            Claims {
                thread_id: "t-a".into(),
                run_id: "r-1".into(),
                root: "t-a".into(),
                depth: 0,
                turn: "cookie-1".into(),
            }
        }

        /// The round trip, and the shape on the wire.
        #[test]
        fn a_minted_token_verifies_under_the_same_key() {
            let k = Key::from_bytes([1; 32]);
            let t = k.mint(&claims());
            assert!(t.starts_with("rt1."), "{t}");
            assert_eq!(t.split('.').count(), 3, "{t}");
            assert_eq!(k.verify(&t).unwrap(), claims());
            // The floor can read the claims without the key.
            assert_eq!(super::super::peek_unverified(&t).unwrap(), claims());
        }

        /// **A TOKEN MINTED WITH ANOTHER KEY IS REFUSED** — the forged case.
        ///
        /// **Forced red** by making `verify` skip the compare (`if false &&
        /// !constant_time_eq(…)`): the forged token comes back `Ok(claims)`.
        #[test]
        fn a_token_minted_with_another_key_is_refused() {
            let real = Key::from_bytes([1; 32]);
            let forged = Key::from_bytes([2; 32]).mint(&claims());
            assert_eq!(real.verify(&forged), Err(TokenError::BadSignature));
            assert_eq!(
                TokenError::BadSignature.to_string(),
                "bad signature: the token was not minted by this daemon's key, or its claims \
                 were altered after minting"
            );
        }

        /// **ALTERED CLAIMS UNDER A GENUINE MAC ARE REFUSED** — the other-thread
        /// case: take thread A's token and rewrite the claims to say thread B.
        ///
        /// **Forced red** by the same mutation as above: the rewritten token
        /// comes back `Ok` claiming to be `t-b`.
        #[test]
        fn claims_rewritten_to_name_another_thread_are_refused() {
            let k = Key::from_bytes([1; 32]);
            let a = k.mint(&claims());
            let mut other = claims();
            other.thread_id = "t-b".into();
            let other_hex = hex_encode(&serde_json::to_vec(&other).unwrap());
            let parts: Vec<&str> = a.split('.').collect();
            let tampered = format!("{}.{}.{}", parts[0], other_hex, parts[2]);
            assert_eq!(k.verify(&tampered), Err(TokenError::BadSignature));
            // …and the mac moved to the tampered claims, with A's mac, is the same refusal.
            let tampered = format!("{}.{}.{}", parts[0], parts[1], hex_encode(&[0u8; 32]));
            assert_eq!(k.verify(&tampered), Err(TokenError::BadSignature));
        }

        #[test]
        fn malformed_tokens_are_refused_by_shape_before_any_signature_is_checked() {
            let k = Key::from_bytes([1; 32]);
            for bad in ["", "rt1", "rt1.aa", "rt1.aa.bb.cc", "rt2.aa.bb", "rt1..bb", "rt1.aa."] {
                assert!(
                    matches!(k.verify(bad), Err(TokenError::Malformed(_))),
                    "{bad:?}: {:?}",
                    k.verify(bad)
                );
            }
            // Non-hex in the mac: malformed, not a signature failure.
            assert!(matches!(k.verify("rt1.aa.zz"), Err(TokenError::Malformed(_))));
        }

        /// A key file is read back byte for byte, minted when absent, and
        /// refused when it is the wrong length.
        #[test]
        fn the_key_is_minted_once_and_read_back() {
            let dir = std::env::temp_dir().join(format!("shapewire-token-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("run-token.key");
            let a = Key::load_or_mint(&path).unwrap();
            let b = Key::load_or_mint(&path).unwrap();
            assert_eq!(a, b, "the second load reads the first mint");
            assert_eq!(std::fs::read(&path).unwrap().len(), Key::LEN);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "the key is private to the daemon's user");
            }
            std::fs::write(&path, b"short").unwrap();
            let e = Key::load_or_mint(&path).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidData, "{e}");
            assert!(e.to_string().contains("exactly 32"), "{e}");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// RFC 4231 test case 2 — the construction is the textbook one.
        #[test]
        fn hmac_sha256_matches_the_rfc_vector() {
            let mut key = [0u8; 32];
            key[..4].copy_from_slice(b"Jefe");
            let mac = hmac_sha256(&key, b"what do ya want for nothing?");
            assert_eq!(
                hex_encode(&mac),
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
            );
        }
    }
}

#[cfg(feature = "server")]
pub use signed::Key;

#[cfg(test)]
mod floor_tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_refuses_what_is_not_hex() {
        let bytes: Vec<u8> = (0..=255).collect();
        let s = hex_encode(&bytes);
        assert_eq!(s.len(), 512);
        assert_eq!(hex_decode(&s).unwrap(), bytes);
        assert_eq!(hex_decode(&s.to_uppercase()).unwrap(), bytes);
        assert_eq!(hex_decode("abc"), None, "odd length");
        assert_eq!(hex_decode("zz"), None, "not hex");
        assert_eq!(hex_decode(""), Some(Vec::new()));
    }

    /// The claims are exactly five fields; a sixth is refused rather than
    /// ignored, so a minter that starts carrying more cannot be read by a
    /// verifier that silently drops it.
    #[test]
    fn the_claims_are_exactly_five_fields() {
        let json = serde_json::json!({
            "thread_id": "t", "run_id": "r", "root": "t", "depth": 2, "turn": "c"
        });
        let c: Claims = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(c.depth, 2);
        let mut extra = json.clone();
        extra["budget_key"] = serde_json::json!("k");
        assert!(serde_json::from_value::<Claims>(extra).is_err(), "a sixth field is refused");
        let mut short = json;
        short.as_object_mut().unwrap().remove("turn");
        assert!(serde_json::from_value::<Claims>(short).is_err(), "a missing field is refused");
    }

    /// The floor can read a token's claims without a key, and says so in the
    /// function's name.
    #[test]
    fn peek_reads_claims_without_a_key_and_refuses_a_malformed_token() {
        let claims = Claims {
            thread_id: "t".into(),
            run_id: "r".into(),
            root: "t".into(),
            depth: 0,
            turn: "c".into(),
        };
        let payload = hex_encode(&serde_json::to_vec(&claims).unwrap());
        let token = format!("{PREFIX}.{payload}.00");
        assert_eq!(peek_unverified(&token).unwrap(), claims);
        assert!(matches!(peek_unverified("nope"), Err(TokenError::Malformed(_))));
        assert!(matches!(
            peek_unverified(&format!("{PREFIX}.zz.00")),
            Err(TokenError::Malformed(_))
        ));
    }
}
