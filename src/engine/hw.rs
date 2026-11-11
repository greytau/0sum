//! The seat-signed high-water mark (amendment A4). Every prefix of a valid
//! log is valid, so the log alone can't show it was cut back. Each seat keeps
//! `sessions/<SID>.<seat>.hw`: the last head it saw, signed with its own key.
//! A log that no longer contains that head was rolled back, and the seat
//! refuses to play on it. This stops UNPLANNED rollback; a planned snapshot of
//! the whole directory (log and marks together) still beats it on a shared disk.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::log::{Hex, Hex32, Hex64, Session};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HighWater {
    pub seq: u64,
    pub hash: Hex32,
    pub sig: Hex64,
}

/// "0sum/v2/hw\0" ‖ lp(SID) ‖ h0 ‖ u64be(seq) ‖ hash
fn message(s: &Session, seq: u64, hash: &Hex32) -> Vec<u8> {
    let mut m = b"0sum/v2/hw\0".to_vec();
    m.extend_from_slice(&(s.session_id.len() as u16).to_be_bytes());
    m.extend_from_slice(s.session_id.as_bytes());
    m.extend_from_slice(&s.log[0].hash.0);
    m.extend_from_slice(&seq.to_be_bytes());
    m.extend_from_slice(&hash.0);
    m
}

/// A mark at the session's current head.
pub fn mark(s: &Session, sk: &SigningKey) -> HighWater {
    let head = s.log.last().expect("a session has genesis");
    HighWater {
        seq: head.seq,
        hash: head.hash,
        sig: Hex(sk.sign(&message(s, head.seq, &head.hash)).to_bytes()),
    }
}

const DONT: &str = "Don't play on. Run 0sum verify.";

/// Whether `raw` is a mark for this session signed by `key`, wherever it points.
pub fn signed_by(s: &Session, key: &VerifyingKey, raw: &str) -> bool {
    serde_json::from_str::<HighWater>(raw).is_ok_and(|hw| {
        key.verify_strict(
            &message(s, hw.seq, &hw.hash),
            &Signature::from_bytes(&hw.sig.0),
        )
        .is_ok()
    })
}

/// Run after replay, under the lock, before any duty. `raw` is the mark
/// file's contents, if it exists. Err = refuse: append nothing, write nothing.
pub fn check(s: &Session, seat: &str, key: &VerifyingKey, raw: Option<&str>) -> Result<(), String> {
    let Some(raw) = raw else {
        if s.log.iter().any(|e| e.author.as_deref() == Some(seat)) {
            return Err(format!(
                "your high-water mark for {} is missing, but the log holds entries you signed. {DONT}",
                s.session_id
            ));
        }
        return Ok(());
    };
    let hw: HighWater = serde_json::from_str(raw).map_err(|e| {
        format!(
            "your high-water mark for {} doesn't parse ({}). {DONT}",
            s.session_id,
            super::escape(&e.to_string())
        )
    })?;
    key.verify_strict(
        &message(s, hw.seq, &hw.hash),
        &Signature::from_bytes(&hw.sig.0),
    )
    .map_err(|_| {
        format!(
            "your high-water mark for {} isn't signed by you. {DONT}",
            s.session_id
        )
    })?;
    match s.log.get(hw.seq as usize) {
        Some(e) if e.hash == hw.hash => Ok(()),
        _ => Err(format!(
            "ROLLBACK: you last saw seq {}:{}; this log no longer contains it. {DONT}",
            hw.seq,
            &hex::encode(hw.hash.0)[..12]
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testkit::{player, seated};
    use crate::farkle::ruleset::RuleSet;
    use crate::farkle::Farkle;

    fn json(hw: &HighWater) -> String {
        serde_json::to_string(hw).unwrap()
    }

    #[test]
    fn a_mark_at_the_head_passes_and_survives_growth() {
        let (s, _, players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        let key = players[0].sk.verifying_key();
        let m = json(&mark(&s, &players[0].sk));
        check(&s, "p1", &key, Some(&m)).unwrap();
        let mut longer = s.clone();
        longer.log.push(s.log[2].clone());
        check(&longer, "p1", &key, Some(&m)).unwrap();
    }

    #[test]
    fn a_truncated_log_is_a_rollback() {
        let (mut s, _, players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        let m = json(&mark(&s, &players[1].sk));
        let seen = hex::encode(s.log[2].hash.0);
        s.log.truncate(2);
        let e = check(&s, "p2", &players[1].sk.verifying_key(), Some(&m)).unwrap_err();
        assert_eq!(
            e,
            format!("ROLLBACK: you last saw seq 2:{}; this log no longer contains it. Don't play on. Run 0sum verify.", &seen[..12])
        );
    }

    #[test]
    fn missing_bad_or_foreign_marks_refuse() {
        let (s, _, players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        let key = players[0].sk.verifying_key();
        assert!(check(&s, "p1", &key, None)
            .unwrap_err()
            .contains("is missing"));
        assert!(check(&s, "p1", &key, Some("{}"))
            .unwrap_err()
            .contains("doesn't parse"));
        let theirs = json(&mark(&s, &player(1).sk));
        assert!(check(&s, "p1", &key, Some(&theirs))
            .unwrap_err()
            .contains("isn't signed by you"));
        let fresh = &s.log[..2];
        let unsigned_by_me = Session {
            log: fresh.to_vec(),
            ..s.clone()
        };
        let p2 = player(1).sk.verifying_key();
        check(&unsigned_by_me, "p2", &p2, None).unwrap();
    }
}
