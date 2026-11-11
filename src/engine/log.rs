//! The signed, hash-chained log (PHASE2 §3, §8): envelope types, canonical
//! bytes, the chain hash, and entry signatures.

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 2;

/// Fixed-size bytes that (de)serialize as lowercase hex and reject any other
/// length or case.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Hex<const N: usize>(pub [u8; N]);

pub type Hex16 = Hex<16>;
pub type Hex32 = Hex<32>;
pub type Hex64 = Hex<64>;

impl<const N: usize> fmt::Debug for Hex<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl<const N: usize> Serialize for Hex<N> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de, const N: usize> Deserialize<'de> for Hex<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() != 2 * N
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(D::Error::custom(format!(
                "expected {} lowercase hex chars",
                2 * N
            )));
        }
        let mut out = [0u8; N];
        hex::decode_to_slice(&s, &mut out).map_err(D::Error::custom)?;
        Ok(Hex(out))
    }
}

/// The on-disk session file. The log is the only state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub version: u32,
    pub session_id: String,
    pub log: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub seq: u64,
    /// Unix seconds, self-reported, display-only.
    pub ts: u64,
    /// Seat id ("p1"); `None` only for genesis.
    pub author: Option<String>,
    pub prev: Hex32,
    pub body: Body,
    /// Stored for readability; replay recomputes it and requires equality.
    pub hash: Hex32,
    pub sig: Option<Hex64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Body {
    Genesis(Genesis),
    Register(Register),
    /// The game's event, parsed strictly by the game on replay.
    Game(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Genesis {
    pub session_id: String,
    pub game: String,
    pub ruleset: serde_json::Value,
    pub roster: Vec<Seat>,
    /// Makes h0 unique per creation.
    pub nonce: Hex16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seat {
    pub seat: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub seat: String,
    pub pubkey: Hex32,
}

/// Sorted keys at every depth come from `serde_json::Value`'s BTreeMap — which
/// is why `preserve_order` must never be enabled (the genesis vector guards it).
pub fn canonical(seq: u64, ts: u64, author: &Option<String>, prev: &Hex32, body: &Body) -> Vec<u8> {
    let v = serde_json::json!({
        "seq": seq, "ts": ts, "author": author, "prev": prev, "body": body,
    });
    serde_json::to_vec(&v).expect("a Value always serializes")
}

pub fn chain_hash(seq: u64, ts: u64, author: &Option<String>, prev: &Hex32, body: &Body) -> Hex32 {
    let mut h = Sha256::new();
    h.update(b"0sum/v2/entry\0");
    h.update(canonical(seq, ts, author, prev, body));
    Hex(h.finalize().into())
}

fn sig_message(hash: &Hex32) -> Vec<u8> {
    let mut m = b"0sum/v2/sig\0".to_vec();
    m.extend_from_slice(&hash.0);
    m
}

impl Entry {
    pub fn recompute_hash(&self) -> Hex32 {
        chain_hash(self.seq, self.ts, &self.author, &self.prev, &self.body)
    }

    pub fn genesis(ts: u64, genesis: Genesis) -> Entry {
        let body = Body::Genesis(genesis);
        let prev = Hex([0; 32]);
        let hash = chain_hash(0, ts, &None, &prev, &body);
        Entry {
            seq: 0,
            ts,
            author: None,
            prev,
            body,
            hash,
            sig: None,
        }
    }

    pub fn signed(
        seq: u64,
        ts: u64,
        author: &str,
        prev: Hex32,
        body: Body,
        sk: &SigningKey,
    ) -> Entry {
        let author = Some(author.to_string());
        let hash = chain_hash(seq, ts, &author, &prev, &body);
        let sig = sk.sign(&sig_message(&hash)).to_bytes();
        Entry {
            seq,
            ts,
            author,
            prev,
            body,
            hash,
            sig: Some(Hex(sig)),
        }
    }

    /// Strict verification of `sig` over the *recomputed* hash.
    pub fn verify_sig(&self, key: &VerifyingKey) -> Result<(), String> {
        let sig = self.sig.ok_or("entry is unsigned")?;
        key.verify_strict(
            &sig_message(&self.recompute_hash()),
            &Signature::from_bytes(&sig.0),
        )
        .map_err(|_| "signature doesn't verify under the author's key".to_string())
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::keys::{Password, Secrets};
    use serde_json::json;

    fn vector_genesis() -> Entry {
        Entry::genesis(
            0,
            Genesis {
                session_id: "TEST01".into(),
                game: "liars-dice".into(),
                ruleset: json!({ "name": "classic", "dice_per_player": 5, "wild_ones": true, "spot_on": false }),
                roster: vec![
                    Seat {
                        seat: "p1".into(),
                        name: "Alice".into(),
                    },
                    Seat {
                        seat: "p2".into(),
                        name: "Bob".into(),
                    },
                ],
                nonce: Hex([0; 16]),
            },
        )
    }

    /// Also the `preserve_order` tripwire: keys must come out sorted.
    #[test]
    fn golden_genesis_canonical_bytes_and_h0() {
        let g = vector_genesis();
        assert_eq!(
            String::from_utf8(canonical(g.seq, g.ts, &g.author, &g.prev, &g.body)).unwrap(),
            r#"{"author":null,"body":{"genesis":{"game":"liars-dice","nonce":"00000000000000000000000000000000","roster":[{"name":"Alice","seat":"p1"},{"name":"Bob","seat":"p2"}],"ruleset":{"dice_per_player":5,"name":"classic","spot_on":false,"wild_ones":true},"session_id":"TEST01"}},"prev":"0000000000000000000000000000000000000000000000000000000000000000","seq":0,"ts":0}"#
        );
        assert_eq!(
            hex::encode(g.hash.0),
            "4c3c06def777ecf293af7b1fa324f46422ef4d137f8a0bcab71dd943d7040dee"
        );
    }

    #[test]
    fn golden_register_h1_and_sig1() {
        let g = vector_genesis();
        let sk =
            Secrets::from_password(&Password::parse("TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap())
                .signing_key();
        let reg = Entry::signed(
            1,
            0,
            "p1",
            g.hash,
            Body::Register(Register {
                seat: "p1".into(),
                pubkey: Hex(sk.verifying_key().to_bytes()),
            }),
            &sk,
        );
        assert_eq!(
            hex::encode(reg.hash.0),
            "2c0830ef6065e388e48a5db116d0acd630386b8d6c936612070ddc4eb128cd2c"
        );
        assert_eq!(
            hex::encode(reg.sig.unwrap().0),
            "7e9f9f475e42da3e8a3724967ce7c540d8025bae70c4eada55b7a6196e44449d882b39b8fdfbe44119220aa01105fec20a4e8e0a229684dbbb44116fd28bf201"
        );
        reg.verify_sig(&sk.verifying_key()).unwrap();
    }

    #[test]
    fn hex_rejects_wrong_case_and_length() {
        let ok: Hex16 = serde_json::from_str(r#""000102030405060708090a0b0c0d0e0f""#).unwrap();
        assert_eq!(ok.0[15], 15);
        assert!(serde_json::from_str::<Hex16>(r#""000102030405060708090A0B0C0D0E0F""#).is_err());
        assert!(serde_json::from_str::<Hex16>(r#""0001""#).is_err());
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#""000102030405060708090a0b0c0d0e0f""#
        );
    }

    #[test]
    fn entry_rejects_unknown_fields() {
        let mut v = serde_json::to_value(vector_genesis()).unwrap();
        v["extra"] = json!(1);
        assert!(serde_json::from_value::<Entry>(v).is_err());
    }
}
