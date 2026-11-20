//! Board encodings and geometry (B§2.1, B§10 item 6). Pure functions shared by
//! `plan`, `duty`, `fold` and the renderers, so live play and verify run the
//! same geometry. [`squares`] is the only code that turns a placement into
//! squares (amendment A1).

use rand::Rng;
use serde::{Deserialize, Serialize};

use super::ruleset::RuleSet;
use super::Cheat;
use crate::engine::escape;
use crate::engine::keys::Secrets;
use crate::engine::slots::{self, Scope};

pub const SHIP: &str = "battleship/ship";
pub const CELL: &str = "battleship/cell";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir {
    #[serde(rename = "h")]
    H,
    #[serde(rename = "v")]
    V,
}

impl Dir {
    pub fn letter(self) -> &'static str {
        match self {
            Dir::H => "h",
            Dir::V => "v",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Dir::H => "across",
            Dir::V => "down",
        }
    }
}

/// A ship's origin (its top/left square) and direction. Rows and columns are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub row: u8,
    pub col: u8,
    pub dir: Dir,
}

impl Placement {
    /// The ship slot's value: `[row, col, dir]`, dir 0 = h, 1 = v.
    pub fn enc(&self) -> [u8; 3] {
        [self.row, self.col, self.dir as u8]
    }

    /// `C3 v`.
    pub fn short(&self) -> String {
        format!(
            "{} {}",
            rc_name(self.row as u16, self.col as u16),
            self.dir.letter()
        )
    }
}

/// b: the board seed, from the password only.
pub fn seed(secrets: &Secrets) -> [u8; 32] {
    secrets.derive(b"0sum/v2/battleship/board")
}

/// A1: on the board iff row < rows, col < cols, and the far end is too,
/// computed in u16. `None` for anything else; no index exists before this.
pub fn squares(rows: u8, cols: u8, p: Placement, n: u8) -> Option<Vec<u16>> {
    let (rows, cols) = (rows as u16, cols as u16);
    let (r, c, n) = (p.row as u16, p.col as u16, n as u16);
    let fits = r < rows
        && c < cols
        && match p.dir {
            Dir::H => c + n <= cols,
            Dir::V => r + n <= rows,
        };
    fits.then(|| {
        (0..n)
            .map(|t| match p.dir {
                Dir::H => r * cols + c + t,
                Dir::V => (r + t) * cols + c,
            })
            .collect()
    })
}

pub fn ship_squares(rules: &RuleSet, k: usize, p: Placement) -> Option<Vec<u16>> {
    squares(rules.rows, rules.cols, p, rules.fleet[k].len)
}

/// The lowest square two ships share.
pub fn first_shared(a: &[u16], b: &[u16]) -> Option<u16> {
    a.iter().filter(|i| b.contains(i)).min().copied()
}

/// Any square of `a` 8-adjacent to (or on) a square of `b`.
pub fn adjacent(cols: u8, a: &[u16], b: &[u16]) -> bool {
    let cols = cols as i32;
    let rc = |i: u16| (i as i32 / cols, i as i32 % cols);
    a.iter().any(|&i| {
        let (r, c) = rc(i);
        b.iter().any(|&j| {
            let (s, d) = rc(j);
            (r - s).abs() <= 1 && (c - d).abs() <= 1
        })
    })
}

/// `B5`. A row past Z (only a doctored placement has one) is spelled out.
pub fn rc_name(r: u16, c: u16) -> String {
    if r < 26 {
        format!("{}{}", (b'A' + r as u8) as char, c + 1)
    } else {
        format!("row {} col {}", r + 1, c + 1)
    }
}

pub fn sq_name(cols: u8, i: u16) -> String {
    let cols = cols.max(1) as u16;
    rc_name(i / cols, i % cols)
}

fn bounds(rules: &RuleSet) -> String {
    format!(
        "rows A-{}, columns 1-{}",
        (b'A' + rules.rows - 1) as char,
        rules.cols
    )
}

/// Shape only: a letter then 1-2 digits; bounds are parse_square's.
pub fn looks_like_square(token: &str) -> bool {
    let b = token.as_bytes();
    (2..=3).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(u8::is_ascii_digit)
}

pub fn parse_square(rules: &RuleSet, token: &str) -> Result<(u8, u8), String> {
    if !looks_like_square(token) {
        return Err(format!(
            "`{}` isn't a square ({}, e.g. B5)",
            escape(token),
            bounds(rules)
        ));
    }
    let up = token.to_ascii_uppercase();
    let row = up.as_bytes()[0] - b'A';
    let num: u8 = up[1..].parse().expect("1-2 ascii digits");
    if row >= rules.rows || num == 0 || num > rules.cols {
        return Err(format!(
            "`{}` isn't on the board ({})",
            escape(token),
            bounds(rules)
        ));
    }
    Ok((row, num - 1))
}

pub fn ship_scope<'a>(sid: &'a str, seat: &'a str) -> Scope<'a> {
    Scope {
        label: SHIP,
        sid,
        seat,
    }
}

pub fn cell_scope<'a>(sid: &'a str, seat: &'a str) -> Scope<'a> {
    Scope {
        label: CELL,
        sid,
        seat,
    }
}

pub fn ship_commitment(scope: &Scope, b: &[u8; 32], k: usize, p: Placement) -> [u8; 32] {
    let k = k as u16;
    slots::commit(scope, k, &slots::salt(b, SHIP, k), &p.enc())
}

/// B§3.2: each placed ship recovered by trying every on-board origin and
/// direction against our own salts. `Err(k)` names the first ship that
/// doesn't open under this seed.
pub fn read_back(
    rules: &RuleSet,
    scope: &Scope,
    b: &[u8; 32],
    placed: &[Option<[u8; 32]>],
) -> Result<Vec<Option<Placement>>, usize> {
    placed
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let Some(c) = c else { return Ok(None) };
            let salt = slots::salt(b, SHIP, k as u16);
            for row in 0..rules.rows {
                for col in 0..rules.cols {
                    for dir in [Dir::H, Dir::V] {
                        let p = Placement { row, col, dir };
                        if ship_squares(rules, k, p).is_some()
                            && slots::opens(scope, c, k as u16, &salt, &p.enc())
                        {
                            return Ok(Some(p));
                        }
                    }
                }
            }
            Err(k)
        })
        .collect()
}

/// Occupancy of a fleet already known to be on the board.
pub fn occupancy(rules: &RuleSet, fleet: &[Placement]) -> Vec<u8> {
    let mut occ = vec![0; rules.cells() as usize];
    for (k, p) in fleet.iter().enumerate() {
        for i in ship_squares(rules, k, *p).expect("a planned ship is on the board") {
            occ[i as usize] = 1;
        }
    }
    occ
}

/// Why ship `k` can't go at `p` among `cur`'s other ships, in words.
pub fn check_place(
    rules: &RuleSet,
    cur: &[Option<Placement>],
    k: usize,
    p: Placement,
) -> Result<(), String> {
    let name = &rules.fleet[k].name;
    let len = rules.fleet[k].len;
    let at = p.short();
    let Some(mine) = ship_squares(rules, k, p) else {
        let end = match p.dir {
            Dir::H => format!("column {}", rules.cols),
            Dir::V => format!("row {}", (b'A' + rules.rows - 1) as char),
        };
        return Err(format!(
            "the {name} doesn't fit at {} {} ({len} long; the board ends at {end})",
            rc_name(p.row as u16, p.col as u16),
            p.dir.word()
        ));
    };
    for (m, q) in cur.iter().enumerate() {
        let Some(q) = q.filter(|_| m != k) else {
            continue;
        };
        let theirs = ship_squares(rules, m, q).expect("a planned ship is on the board");
        if let Some(j) = first_shared(&mine, &theirs) {
            return Err(format!(
                "the {name} at {at} overlaps your {} at {}",
                rules.fleet[m].name,
                sq_name(rules.cols, j)
            ));
        }
        if !rules.touching && adjacent(rules.cols, &mine, &theirs) {
            return Err(format!(
                "the {name} at {at} touches your {} (ruleset {} keeps ships apart)",
                rules.fleet[m].name, rules.name
            ));
        }
    }
    Ok(())
}

/// B§3.3: fill every unplaced ship, in fleet order, uniformly among the
/// legal spots left; all placed means re-roll the whole fleet.
pub fn auto(
    rules: &RuleSet,
    start: &[Option<Placement>],
    rng: &mut impl Rng,
) -> Result<Vec<Placement>, String> {
    let base: Vec<Option<Placement>> = if start.iter().all(Option::is_some) {
        vec![None; start.len()]
    } else {
        start.to_vec()
    };
    'attempt: for _ in 0..100 {
        let mut cur = base.clone();
        for k in 0..cur.len() {
            if cur[k].is_some() {
                continue;
            }
            let mut spots = Vec::new();
            for row in 0..rules.rows {
                for col in 0..rules.cols {
                    for dir in [Dir::H, Dir::V] {
                        let p = Placement { row, col, dir };
                        if check_place(rules, &cur, k, p).is_ok() {
                            spots.push(p);
                        }
                    }
                }
            }
            if spots.is_empty() {
                continue 'attempt;
            }
            cur[k] = Some(spots[rng.gen_range(0..spots.len())]);
        }
        return Ok(cur.into_iter().map(|p| p.expect("all placed")).collect());
    }
    Err("can't fit the remaining ships around the ones you placed; re-place some by hand".into())
}

/// B§6.4's legality, in order: on the board, no overlaps, no touching (if
/// the rules say so), then the committed occupancy against the ships square
/// by square. `Withheld` needs the shots and is the caller's.
pub fn legality(rules: &RuleSet, fleet: &[Placement], occ: &[u8]) -> Result<(), Cheat> {
    let mut ships = Vec::new();
    for (k, p) in fleet.iter().enumerate() {
        match ship_squares(rules, k, *p) {
            Some(sq) => ships.push(sq),
            None => {
                return Err(Cheat::OffBoard {
                    ship: k as u8,
                    at: *p,
                })
            }
        }
    }
    for k in 0..ships.len() {
        for m in k + 1..ships.len() {
            if let Some(cell) = first_shared(&ships[k], &ships[m]) {
                return Err(Cheat::Overlap {
                    ship: k as u8,
                    other: m as u8,
                    cell,
                });
            }
        }
    }
    if !rules.touching {
        for k in 0..ships.len() {
            for m in k + 1..ships.len() {
                if adjacent(rules.cols, &ships[k], &ships[m]) {
                    return Err(Cheat::Touching {
                        ship: k as u8,
                        other: m as u8,
                    });
                }
            }
        }
    }
    for (i, &o) in occ.iter().enumerate() {
        let i = i as u16;
        match ships.iter().position(|sq| sq.contains(&i)) {
            None if o == 1 => return Err(Cheat::Phantom { cell: i }),
            Some(k) if o == 0 => {
                return Err(Cheat::Hidden {
                    ship: k as u8,
                    cell: i,
                })
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn at(row: u8, col: u8, dir: Dir) -> Placement {
        Placement { row, col, dir }
    }

    #[test]
    fn squares_stay_on_the_board_and_never_bend() {
        assert_eq!(
            squares(10, 10, at(0, 0, Dir::H), 5),
            Some(vec![0, 1, 2, 3, 4])
        );
        assert_eq!(
            squares(10, 10, at(2, 2, Dir::V), 5),
            Some(vec![22, 32, 42, 52, 62])
        );
        assert_eq!(
            squares(10, 10, at(9, 6, Dir::H), 4),
            Some(vec![96, 97, 98, 99])
        );
        // A8 h: the naive index math would bend onto row B.
        assert_eq!(squares(10, 10, at(0, 7, Dir::H), 5), None);
        assert_eq!(squares(10, 10, at(6, 0, Dir::V), 5), None);
        assert_eq!(squares(10, 10, at(255, 0, Dir::H), 5), None);
        assert_eq!(squares(10, 10, at(0, 255, Dir::V), 2), None);
        assert_eq!(squares(10, 10, at(255, 255, Dir::V), 255), None);
        assert_eq!(
            squares(10, 10, at(0, 5, Dir::H), 5),
            Some(vec![5, 6, 7, 8, 9])
        );
    }

    #[test]
    fn adjacency_includes_diagonals() {
        assert!(adjacent(10, &[0], &[11]));
        assert!(adjacent(10, &[0], &[1]));
        assert!(!adjacent(10, &[0], &[2]));
        // Column 10 and column 1 of the next row are not neighbours.
        assert!(!adjacent(10, &[9], &[10]));
    }

    #[test]
    fn squares_parse_case_insensitively_within_bounds() {
        let r = RuleSet::classic();
        assert_eq!(parse_square(&r, "a1"), Ok((0, 0)));
        assert_eq!(parse_square(&r, "J10"), Ok((9, 9)));
        assert_eq!(
            parse_square(&r, "k1").unwrap_err(),
            "`k1` isn't on the board (rows A-J, columns 1-10)"
        );
        assert!(parse_square(&r, "A11")
            .unwrap_err()
            .contains("isn't on the board"));
        assert!(parse_square(&r, "A0")
            .unwrap_err()
            .contains("isn't on the board"));
        assert!(parse_square(&r, "A100")
            .unwrap_err()
            .contains("isn't a square"));
        assert_eq!(sq_name(10, 44), "E5");
    }

    #[test]
    fn auto_placement_is_legal_under_both_rulesets() {
        for rules in [RuleSet::classic(), RuleSet::notouch()] {
            for seed in 0..20 {
                let mut rng = StdRng::seed_from_u64(seed);
                let fleet = auto(&rules, &[None; 5], &mut rng).unwrap();
                let occ = occupancy(&rules, &fleet);
                assert_eq!(legality(&rules, &fleet, &occ), Ok(()));
                assert_eq!(occ.iter().filter(|&&o| o == 1).count(), 17);
            }
        }
    }

    #[test]
    fn auto_fills_around_placed_ships_and_rerolls_a_full_fleet() {
        let rules = RuleSet::classic();
        let mut rng = StdRng::seed_from_u64(3);
        let mut start = [None; 5];
        start[1] = Some(at(4, 4, Dir::V));
        let fleet = auto(&rules, &start, &mut rng).unwrap();
        assert_eq!(fleet[1], at(4, 4, Dir::V));
        let full: Vec<Option<Placement>> = fleet.iter().copied().map(Some).collect();
        let again = auto(&rules, &full, &mut rng).unwrap();
        assert_ne!(again, fleet);
    }

    #[test]
    fn auto_gives_up_when_nothing_fits() {
        let mut rules = RuleSet::classic();
        rules.rows = 2;
        rules.cols = 5;
        let mut rng = StdRng::seed_from_u64(1);
        let mut start = [None; 5];
        start[0] = Some(at(0, 0, Dir::H));
        assert_eq!(
            auto(&rules, &start, &mut rng).unwrap_err(),
            "can't fit the remaining ships around the ones you placed; re-place some by hand"
        );
    }
}
