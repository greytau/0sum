//! Fair hidden dice (PHASE2 §4.1, amendment A3): commitments, the commitment
//! set, verifiable entropy, and the rejection-sampled dice stream. Pure
//! functions over bytes; every verifier reproduces them exactly.

use sha2::{Digest, Sha256};

use crate::engine::keys::Secrets;

fn lp(h: &mut Sha256, x: &str) {
    h.update((x.len() as u16).to_be_bytes());
    h.update(x.as_bytes());
}

fn bound(domain: &[u8], sid: &str, round: u32, seat: &str, s: &[u8; 32]) -> Sha256 {
    let mut h = Sha256::new();
    h.update(domain);
    lp(&mut h, sid);
    h.update(round.to_be_bytes());
    lp(&mut h, seat);
    h.update(s);
    h
}

/// s_i^r: the per-round secret, from the password only.
pub fn secret(secrets: &Secrets, round: u32) -> [u8; 32] {
    let mut info = b"0sum/v2/liars-dice/secret".to_vec();
    info.extend_from_slice(&round.to_be_bytes());
    secrets.derive(&info)
}

/// C_i^r = SHA-256("0sum/v2/liars-dice/commit\0" ‖ lp(SID) ‖ u32be(r) ‖ lp(seat) ‖ s).
pub fn commitment(sid: &str, round: u32, seat: &str, s: &[u8; 32]) -> [u8; 32] {
    bound(b"0sum/v2/liars-dice/commit\0", sid, round, seat, s)
        .finalize()
        .into()
}

/// CSET^r = SHA-256 of every live seat's commitment, in seat order.
pub fn commitment_set<'a>(commits: impl IntoIterator<Item = &'a [u8; 32]>) -> [u8; 32] {
    let mut h = Sha256::new();
    for c in commits {
        h.update(c);
    }
    h.finalize().into()
}

/// n_i^r (A3): checkable against the revealed `s`, so a published entropy
/// value is provably the one the seat's secret and the commitment set fix.
pub fn entropy(sid: &str, round: u32, seat: &str, s: &[u8; 32], cset: &[u8; 32]) -> [u8; 32] {
    let mut h = bound(b"0sum/v2/liars-dice/entropy\0", sid, round, seat, s);
    h.update(cset);
    h.finalize().into()
}

/// The seat's dice: a PRF of its own `s` and everyone else's entropy, walked
/// with rejection sampling (252 = 6 × 42) so no face is favoured.
pub fn roll<'a>(
    sid: &str,
    round: u32,
    seat: &str,
    s: &[u8; 32],
    others: impl IntoIterator<Item = &'a [u8; 32]>,
    count: u8,
) -> Vec<u8> {
    let mut h = bound(b"0sum/v2/liars-dice/dice\0", sid, round, seat, s);
    for n in others {
        h.update(n);
    }
    let d: [u8; 32] = h.finalize().into();
    let mut dice = Vec::with_capacity(count as usize);
    let mut j: u32 = 0;
    while dice.len() < count as usize {
        let block: [u8; 32] = Sha256::new()
            .chain_update(d)
            .chain_update(j.to_be_bytes())
            .finalize()
            .into();
        for b in block {
            if dice.len() == count as usize {
                break;
            }
            if b < 252 {
                dice.push(b % 6 + 1);
            }
        }
        j += 1;
    }
    dice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::keys::Password;

    fn s1(pw: &str) -> [u8; 32] {
        secret(&Secrets::from_password(&Password::parse(pw).unwrap()), 1)
    }

    /// PHASE2 §13 vectors, as amended by A3. Cross-checked independently by
    /// tests/vectors/ld_vectors.py (Python stdlib, written from the spec).
    #[test]
    fn golden_round_1_vectors() {
        let s_a = s1("TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA");
        let s_b = s1("TEST01-BBBBBBBBBBBBBBBBBBBBBBBBBB");
        let c_a = commitment("TEST01", 1, "p1", &s_a);
        let c_b = commitment("TEST01", 1, "p2", &s_b);
        let cset = commitment_set([&c_a, &c_b]);
        let n_a = entropy("TEST01", 1, "p1", &s_a, &cset);
        let n_b = entropy("TEST01", 1, "p2", &s_b, &cset);
        let hx = hex::encode;
        assert_eq!(
            hx(s_a),
            "612f661662e64504ef80d2ce7dcb3171d12fb3a73733e272eab3a79a18a6efb3"
        );
        assert_eq!(
            hx(s_b),
            "24480e09e67297d9b0c2cf278727d3df6c636c2d122d01a7ed6f47421bd8ad9e"
        );
        assert_eq!(
            hx(c_a),
            "d7f50e01f67658bface606486dff390228e09589e4ae46f60295ba7c6a196128"
        );
        assert_eq!(
            hx(c_b),
            "9dddb09c7c16cc8a2cba8b6db9ec56469d97580477b6da97f264d93f4b1c72dd"
        );
        assert_eq!(
            hx(cset),
            "2998cfcb2d78a9012bdcc6a131d8a4bf04e11939cfc0f7b8d89806e4582614f0"
        );
        assert_eq!(
            hx(n_a),
            "4b5815a8afe6b4db7eda09da30246c1cc116dc66476f6b11345420c3dd265d7c"
        );
        assert_eq!(
            hx(n_b),
            "7470013c07f5eaf7d2f31f1d2331afb684ee7f4120695e27c4a4264fb7eb5797"
        );
        assert_eq!(roll("TEST01", 1, "p1", &s_a, [&n_b], 5), [4, 3, 4, 1, 4]);
        assert_eq!(roll("TEST01", 1, "p2", &s_b, [&n_a], 5), [5, 4, 1, 4, 1]);
    }

    #[test]
    fn rejection_sampling_skips_high_bytes() {
        // Every byte value maps to its face or is skipped, never biased.
        let faces: Vec<u8> = (0u16..256)
            .filter(|&b| b < 252)
            .map(|b| (b % 6 + 1) as u8)
            .collect();
        for f in 1..=6 {
            assert_eq!(faces.iter().filter(|&&d| d == f).count(), 42);
        }
        let d = roll("TEST01", 1, "p1", &[0; 32], [], 200);
        assert_eq!(d.len(), 200);
        assert!(d.iter().all(|f| (1..=6).contains(f)));
    }
}
