//! Identity (PHASE2 §2): a binary-minted password → HKDF → signing key and
//! per-game secrets. Nothing here is ever written to disk.

use std::fmt;

use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;

/// Crockford base32 — unambiguous, hand-writable (no I/L/O/U).
pub const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const SID_LEN: usize = 6;
pub const SECRET_LEN: usize = 26;

/// 32 is a power of two, so masking a u32 is unbiased.
pub fn random_crockford(len: usize) -> String {
    (0..len)
        .map(|_| ALPHABET[(OsRng.next_u32() & 31) as usize] as char)
        .collect()
}

fn is_crockford(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| ALPHABET.contains(&b))
}

/// Normalize and validate a public session id (also guards the path it names).
pub fn parse_sid(raw: &str) -> Result<String, String> {
    let sid = raw.trim().to_ascii_uppercase();
    if is_crockford(&sid, SID_LEN) {
        Ok(sid)
    } else {
        Err(format!(
            "`{}` is not a session id (6 chars of 0-9 A-Z, no I L O U)",
            raw.trim()
        ))
    }
}

/// The §2.5 tripwire shape: `^[0-9A-Z]{6}-[0-9A-Z]{26}$`, case-insensitive.
pub fn looks_like_password(token: &str) -> bool {
    let up = token.to_ascii_uppercase();
    match up.split_once('-') {
        Some((a, b)) => {
            a.len() == SID_LEN
                && b.len() == SECRET_LEN
                && a.bytes()
                    .chain(b.bytes())
                    .all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

pub struct Password {
    sid: String,
    secret: String,
}

impl Password {
    /// The only way a password comes into existence (§2.3: users never choose one).
    pub fn generate(sid: &str) -> Password {
        Password {
            sid: sid.to_string(),
            secret: random_crockford(SECRET_LEN),
        }
    }

    pub fn parse(raw: &str) -> Result<Password, String> {
        let up = raw.trim().to_ascii_uppercase();
        let bad = || {
            "ZEROSUM_PASSWORD isn't a 0sum password (expected SID-SECRET, as printed by `0sum register`)".to_string()
        };
        let (sid, secret) = up.split_once('-').ok_or_else(bad)?;
        if !is_crockford(sid, SID_LEN) || !is_crockford(secret, SECRET_LEN) {
            return Err(bad());
        }
        Ok(Password {
            sid: sid.to_string(),
            secret: secret.to_string(),
        })
    }

    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// The full secret string, for printing exactly once at `register`.
    pub fn expose(&self) -> String {
        format!("{}-{}", self.sid, self.secret)
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-<redacted>", self.sid)
    }
}

/// The password-derived handle games draw on (§9.3). Secrets flow into
/// producing events and rendering, never into validating them.
pub struct Secrets {
    prk: Hkdf<Sha256>,
}

impl Secrets {
    pub fn from_password(pw: &Password) -> Secrets {
        let salt = format!("0sum/v2/{}", pw.sid);
        Secrets {
            prk: Hkdf::<Sha256>::new(Some(salt.as_bytes()), pw.secret.as_bytes()),
        }
    }

    fn expand(&self, info: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        self.prk
            .expand(info, &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 length");
        out
    }

    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.expand(b"0sum/v2/sign"))
    }

    /// A game secret. `info` must be `0sum/v2/<game-id>/...`, which also keeps
    /// it disjoint from the engine's own labels (`0sum/v2/sign`, `0sum/v2/state-key`).
    pub fn derive(&self, info: &[u8]) -> [u8; 32] {
        let rest = info
            .strip_prefix(b"0sum/v2/".as_slice())
            .expect("game secret info must start with 0sum/v2/<game-id>/");
        assert!(
            rest.iter().position(|&b| b == b'/').is_some_and(|i| i > 0),
            "game secret info must start with 0sum/v2/<game-id>/"
        );
        self.expand(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA";
    const P2: &str = "TEST01-BBBBBBBBBBBBBBBBBBBBBBBBBB";

    fn secrets(pw: &str) -> Secrets {
        Secrets::from_password(&Password::parse(pw).unwrap())
    }

    #[test]
    fn golden_sign_seed_and_pubkey() {
        let p1 = secrets(P1).signing_key();
        assert_eq!(
            hex::encode(p1.to_bytes()),
            "06e3cefdb9a2f24c18d37044730b9158bd14fcfe924533028d534a1779f28320"
        );
        assert_eq!(
            hex::encode(p1.verifying_key().to_bytes()),
            "c14f1c546e873389fc5442bc5379bae199b5a514b6cdea5e4a5914ca01d28178"
        );
        let p2 = secrets(P2).signing_key();
        assert_eq!(
            hex::encode(p2.to_bytes()),
            "edab3f381eaa898c8d2b40af741b7313bb5712b428dc05a5f1a32b2e047284e0"
        );
        assert_eq!(
            hex::encode(p2.verifying_key().to_bytes()),
            "53186d38e72218ba73d8db682147897eb029c4d298bfdf56e4a5ef3c8206f2e7"
        );
    }

    #[test]
    fn golden_game_secret_derivation() {
        let mut info = b"0sum/v2/liars-dice/secret".to_vec();
        info.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            hex::encode(secrets(P1).derive(&info)),
            "612f661662e64504ef80d2ce7dcb3171d12fb3a73733e272eab3a79a18a6efb3"
        );
        assert_eq!(
            hex::encode(secrets(P2).derive(&info)),
            "24480e09e67297d9b0c2cf278727d3df6c636c2d122d01a7ed6f47421bd8ad9e"
        );
    }

    #[test]
    #[should_panic(expected = "0sum/v2/<game-id>/")]
    fn derive_refuses_engine_labels() {
        secrets(P1).derive(b"0sum/v2/sign");
    }

    #[test]
    fn password_parse_normalizes_and_validates() {
        let pw = Password::parse("  test01-aaaaaaaaaaaaaaaaaaaaaaaaaa\n").unwrap();
        assert_eq!(pw.expose(), P1);
        assert_eq!(pw.sid(), "TEST01");
        assert!(Password::parse("TEST01-AAAA").is_err());
        assert!(Password::parse("TEST0I-AAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());
        assert!(Password::parse("TEST01AAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());
        assert!(Password::parse("TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAU").is_err());
    }

    #[test]
    fn generated_passwords_parse_back() {
        let pw = Password::generate("TEST01");
        let back = Password::parse(&pw.expose()).unwrap();
        assert_eq!(back.expose(), pw.expose());
        assert_ne!(Password::generate("TEST01").expose(), pw.expose());
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let pw = Password::parse(P1).unwrap();
        assert_eq!(format!("{pw:?}"), "TEST01-<redacted>");
    }

    #[test]
    fn password_tripwire_shape() {
        assert!(looks_like_password(P1));
        assert!(looks_like_password("test01-aaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(looks_like_password("ILOU01-IIIIIIIIIIIIIIIIIIIIIIIIII"));
        assert!(!looks_like_password("roll"));
        assert!(!looks_like_password("TEST01-AAAA"));
    }

    #[test]
    fn sid_parse_guards_paths() {
        assert_eq!(parse_sid(" test01 ").unwrap(), "TEST01");
        assert!(parse_sid("../etc").is_err());
        assert!(parse_sid("TEST0").is_err());
    }
}
