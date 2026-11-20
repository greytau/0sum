//! The slot commitment (docs/BATTLESHIP.md B§2): one seat commits to an array
//! of values, opens slots one at a time in public, and finally reveals the
//! seed so the whole array can be audited. Pure functions over bytes.

use sha2::{Digest, Sha256};

pub struct Scope<'a> {
    pub label: &'a str,
    pub sid: &'a str,
    pub seat: &'a str,
}

fn lp(h: &mut Sha256, x: &[u8]) {
    h.update((x.len() as u16).to_be_bytes());
    h.update(x);
}

/// salt_j = SHA-256("0sum/v2/slot-salt\0" ‖ lp(label) ‖ seed ‖ u16be(j)).
pub fn salt(seed: &[u8; 32], label: &str, j: u16) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"0sum/v2/slot-salt\0");
    lp(&mut h, label.as_bytes());
    h.update(seed);
    h.update(j.to_be_bytes());
    h.finalize().into()
}

/// C_j = SHA-256("0sum/v2/slot\0" ‖ lp(label) ‖ lp(SID) ‖ lp(seat) ‖ u16be(j) ‖ salt_j ‖ lp(v_j)).
pub fn commit(scope: &Scope, j: u16, salt: &[u8; 32], value: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"0sum/v2/slot\0");
    lp(&mut h, scope.label.as_bytes());
    lp(&mut h, scope.sid.as_bytes());
    lp(&mut h, scope.seat.as_bytes());
    h.update(j.to_be_bytes());
    h.update(salt);
    lp(&mut h, value);
    h.finalize().into()
}

pub fn commit_all(scope: &Scope, seed: &[u8; 32], values: &[&[u8]]) -> Vec<[u8; 32]> {
    values
        .iter()
        .enumerate()
        .map(|(j, v)| {
            let j = j as u16;
            commit(scope, j, &salt(seed, scope.label, j), v)
        })
        .collect()
}

pub fn opens(scope: &Scope, c: &[u8; 32], j: u16, salt: &[u8; 32], value: &[u8]) -> bool {
    commit(scope, j, salt, value) == *c
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const SEED: [u8; 32] = [7; 32];

    fn scope<'a>(label: &'a str, sid: &'a str, seat: &'a str) -> Scope<'a> {
        Scope { label, sid, seat }
    }

    #[test]
    fn opens_round_trips_and_binds_everything() {
        let sc = scope("battleship/cell", "TEST01", "p2");
        let s = salt(&SEED, sc.label, 5);
        let c = commit(&sc, 5, &s, &[1]);
        assert!(opens(&sc, &c, 5, &s, &[1]));
        assert!(!opens(&sc, &c, 5, &s, &[0]), "flipped value");
        assert!(!opens(&sc, &c, 6, &s, &[1]), "wrong index");
        assert!(
            !opens(&sc, &c, 5, &salt(&SEED, sc.label, 6), &[1]),
            "wrong salt"
        );
        let other = |sc: Scope| !opens(&sc, &c, 5, &s, &[1]);
        assert!(
            other(scope("battleship/ship", "TEST01", "p2")),
            "wrong label"
        );
        assert!(
            other(scope("battleship/cell", "TEST01", "p1")),
            "wrong seat"
        );
        assert!(other(scope("battleship/cell", "TEST02", "p2")), "wrong SID");
    }

    #[test]
    fn salts_are_pairwise_distinct_across_a_board() {
        let mut seen = HashSet::new();
        for j in 0..5 {
            assert!(seen.insert(salt(&SEED, "battleship/ship", j)));
        }
        for j in 0..100 {
            assert!(seen.insert(salt(&SEED, "battleship/cell", j)));
        }
        assert_eq!(seen.len(), 105);
    }

    #[test]
    fn commit_all_is_commit_per_index() {
        let sc = scope("battleship/cell", "TEST01", "p1");
        let all = commit_all(&sc, &SEED, &[&[0], &[1], &[1]]);
        for (j, v) in [0u8, 1, 1].iter().enumerate() {
            let j = j as u16;
            assert_eq!(
                all[j as usize],
                commit(&sc, j, &salt(&SEED, sc.label, j), &[*v])
            );
        }
    }
}
