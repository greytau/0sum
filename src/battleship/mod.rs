//! Battleship on the engine (docs/BATTLESHIP.md). Each seat commits to its
//! fleet as slot commitments (B§2, B§3); every answer is the opening of the
//! square shot at, so hit and miss can't be lied about, and a proven lie is a
//! forfeit (B§4.3). `fold` checks everything over public data alone.

pub mod board;
pub mod draw;
pub mod ruleset;

use serde::{Deserialize, Serialize};

use crate::engine::log::{Hex, Hex32};
use crate::engine::slots;
use crate::engine::{escape, Ctx, Format, Game, SessionInfo};
use board::{Dir, Placement, CELL, SHIP};
use draw::{
    HowView, Incoming, OwnSquare, OwnView, ShipPart, ShotView, SideStatus, TargetSquare, View,
    ViewPhase,
};
use ruleset::RuleSet;

pub struct Battleship;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Placed {
        ship: u8,
        c: Hex32,
    },
    Fleet {
        cells: Vec<Hex32>,
    },
    Shot {
        cell: u16,
    },
    Answer {
        cell: u16,
        hit: bool,
        salt: Hex32,
        sunk: Option<Sunk>,
    },
    Reveal {
        b: Hex32,
        fleet: Vec<Placement>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sunk {
    pub ship: u8,
    pub salt: Hex32,
    pub at: Placement,
}

/// Ship and square tokens stay raw: resolving them needs the ruleset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Place {
        ship: String,
        square: String,
        dir: Dir,
    },
    PlaceAuto,
    Ready,
    Fire(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Battle,
    Reveal,
    Over,
}

/// One per seat, all public: `boards[d]` is d's own fleet and the shots at it.
#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    /// S_k, latest per ship.
    pub placed: Vec<Option<[u8; 32]>>,
    /// K_i; Some = fleet locked.
    pub cells: Option<Vec<[u8; 32]>>,
    /// Per square, as answered: Some(hit).
    pub shots: Vec<Option<bool>>,
    /// Per ship, once proven.
    pub sunk: Vec<Option<Placement>>,
    pub hits: u16,
    pub misses: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShotRecord {
    pub by: usize,
    pub cell: u16,
    pub hit: Option<bool>,
    pub sunk: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cheat {
    OffBoard { ship: u8, at: Placement },
    Overlap { ship: u8, other: u8, cell: u16 },
    Touching { ship: u8, other: u8 },
    SunkEarly { ship: u8, cell: u16 },
    SunkLate { ship: u8, cell: u16 },
    UnsunkAtFull { sunk: u8 },
    TooManyMisses { misses: u16 },
    Phantom { cell: u16 },
    Hidden { ship: u8, cell: u16 },
    Withheld { ship: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Sank,
    Forfeit { liar: usize, cheat: Cheat },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub winner: usize,
    pub how: How,
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub rules: RuleSet,
    sid: String,
    seats: Vec<String>,
    pub phase: Phase,
    pub boards: Vec<Board>,
    /// The attacker.
    pub turn: usize,
    /// `turn`'s shot at `boards[1 - turn]`, unanswered.
    pub pending: Option<u16>,
    pub history: Vec<ShotRecord>,
    /// Set on entering Reveal: the seat that sank the other's fleet.
    pub provisional: Option<usize>,
    /// Set on entering Over.
    pub outcome: Option<Outcome>,
}

impl State {
    fn ship(&self, k: usize) -> &str {
        &self.rules.fleet[k].name
    }

    fn sq(&self, i: u16) -> String {
        board::sq_name(self.rules.cols, i)
    }

    fn locked(&self, seat: usize) -> bool {
        self.boards[seat].cells.is_some()
    }

    fn unlocked(&self) -> Vec<usize> {
        (0..self.boards.len())
            .filter(|&s| !self.locked(s))
            .collect()
    }

    fn forfeit(&mut self, liar: usize, cheat: Cheat) {
        self.outcome = Some(Outcome {
            winner: 1 - liar,
            how: How::Forfeit { liar, cheat },
        });
        self.phase = Phase::Over;
        self.pending = None;
    }

    fn battle_only(&self) -> Result<(), String> {
        match self.phase {
            Phase::Setup => Err("the battle hasn't started".into()),
            Phase::Reveal => Err("the battle is over: only final reveals now".into()),
            _ => Ok(()),
        }
    }

    fn fleet_names(&self) -> Vec<(String, u8)> {
        fleet_names(&self.rules)
    }
}

fn fleet_names(rules: &RuleSet) -> Vec<(String, u8)> {
    rules
        .fleet
        .iter()
        .map(|s| (s.name.clone(), s.len))
        .collect()
}

fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// B§6.3 step 2: what a sunk opening of ship `k` at `at`, on the answer to
/// shot `i`, proves about its author. `None` = it's a true, timely sunk.
fn sunk_cheat(rules: &RuleSet, b: &Board, k: usize, at: Placement, i: u16) -> Option<Cheat> {
    let ship = k as u8;
    let Some(sq) = board::ship_squares(rules, k, at) else {
        return Some(Cheat::OffBoard { ship, at });
    };
    if let Some(&cell) = sq.iter().find(|&&j| b.shots[j as usize] != Some(true)) {
        return Some(Cheat::SunkEarly { ship, cell });
    }
    if !sq.contains(&i) {
        return Some(Cheat::SunkLate { ship, cell: i });
    }
    let others: Vec<(usize, Vec<u16>)> = b
        .sunk
        .iter()
        .enumerate()
        .filter_map(|(m, p)| {
            let p = (*p)?;
            (m != k).then(|| {
                (
                    m,
                    board::ship_squares(rules, m, p).expect("a proven sunk is on the board"),
                )
            })
        })
        .collect();
    for (m, theirs) in &others {
        if let Some(cell) = board::first_shared(&sq, theirs) {
            return Some(Cheat::Overlap {
                ship,
                other: *m as u8,
                cell,
            });
        }
    }
    if !rules.touching {
        for (m, theirs) in &others {
            if board::adjacent(rules.cols, &sq, theirs) {
                return Some(Cheat::Touching {
                    ship,
                    other: *m as u8,
                });
            }
        }
    }
    None
}

/// B§8.2's forfeit words. `subj`/`poss` name the liar: `Bob`/`Bob's`, or
/// `you`/`your`, or `enemy`/`enemy's` for the ledger.
pub fn cheat_words(
    c: &Cheat,
    cols: u8,
    fleet: &[(String, u8)],
    ruleset: &str,
    subj: &str,
    poss: &str,
) -> String {
    let ship = |k: &u8| {
        fleet
            .get(*k as usize)
            .map_or_else(|| format!("ship #{k}"), |s| s.0.clone())
    };
    let sq = |i: &u16| board::sq_name(cols, *i);
    let fleet_cells: u16 = fleet.iter().map(|s| s.1 as u16).sum();
    match c {
        Cheat::OffBoard { ship: k, at } => format!(
            "{poss} {} runs off the board ({})",
            ship(k),
            at.short()
        ),
        Cheat::Overlap { ship: k, other, cell } => format!(
            "{poss} {} overlaps the {} at {}",
            ship(k),
            ship(other),
            sq(cell)
        ),
        Cheat::Touching { ship: k, other } => format!(
            "{poss} {} touches the {} (ruleset {ruleset})",
            ship(k),
            ship(other)
        ),
        Cheat::SunkEarly { ship: k, cell } => format!(
            "{subj} declared the {} sunk but {} on it was never hit",
            ship(k),
            sq(cell)
        ),
        Cheat::SunkLate { ship: k, cell } => format!(
            "{subj} declared the {} sunk on the shot at {}, which isn't on it: the sunk was held back",
            ship(k),
            sq(cell)
        ),
        Cheat::UnsunkAtFull { sunk } => format!(
            "{fleet_cells} hits on {poss} board but only {sunk} ships declared sunk"
        ),
        Cheat::TooManyMisses { misses } => format!(
            "{misses} misses on {poss} board: it can't hold a {fleet_cells}-square fleet"
        ),
        Cheat::Phantom { cell } => format!(
            "{poss} board marks {} as a ship but no ship is there",
            sq(cell)
        ),
        Cheat::Hidden { ship: k, cell } => format!(
            "{poss} {} covers {} but the board marks it as water",
            ship(k),
            sq(cell)
        ),
        Cheat::Withheld { ship: k } => format!(
            "{poss} {} was hit on every square but never declared sunk",
            ship(k)
        ),
    }
}

fn words_for(st: &State, liar: usize, cheat: &Cheat, names: &[String]) -> String {
    let poss = format!("{}'s", names[liar]);
    cheat_words(
        cheat,
        st.rules.cols,
        &st.fleet_names(),
        &st.rules.name,
        &names[liar],
        &poss,
    )
}

const MOVES_HINT: &str =
    "try: place <ship> <square> <h|v> | place auto | ready | fire <square> | <square>";
const PLACE_USAGE: &str =
    "place takes <ship> <square> <h|v>, e.g. `place carrier A1 h`, or `place auto`";

enum Verb {
    Place,
    Ready,
    Fire,
}

fn as_verb(token: &str) -> Option<Verb> {
    match token.to_ascii_lowercase().as_str() {
        "place" | "p" => Some(Verb::Place),
        "ready" | "lock" | "done" => Some(Verb::Ready),
        "fire" | "f" | "shoot" => Some(Verb::Fire),
        _ => None,
    }
}

fn parse_dir(token: &str) -> Option<Dir> {
    match token.to_ascii_lowercase().as_str() {
        "h" | "across" | "horizontal" => Some(Dir::H),
        "v" | "down" | "vertical" => Some(Dir::V),
        _ => None,
    }
}

/// Exact name or unique prefix, case-insensitive (B§5.1).
fn resolve_ship(rules: &RuleSet, token: &str) -> Result<usize, String> {
    let t = token.to_ascii_lowercase();
    if let Some(k) = rules.fleet.iter().position(|s| s.name == t) {
        return Ok(k);
    }
    let hits: Vec<usize> = (0..rules.fleet.len())
        .filter(|&k| !t.is_empty() && rules.fleet[k].name.starts_with(&t))
        .collect();
    let names: Vec<&str> = rules.fleet.iter().map(|s| s.name.as_str()).collect();
    match hits[..] {
        [k] => Ok(k),
        [] => Err(format!(
            "unknown ship `{}` ({})",
            escape(token),
            names.join(", ")
        )),
        _ => {
            let could: Vec<&str> = hits.iter().map(|&k| names[k]).collect();
            Err(format!(
                "`{}` could be {} — type more letters",
                escape(token),
                could.join(" or ")
            ))
        }
    }
}

fn read_own(st: &State, me: usize, b: &[u8; 32]) -> Result<Vec<Option<Placement>>, String> {
    let scope = board::ship_scope(&st.sid, &st.seats[me]);
    board::read_back(&st.rules, &scope, b, &st.boards[me].placed).map_err(|k| {
        format!(
            "can't read your {} back from the log with this password",
            st.ship(k)
        )
    })
}

fn placed_event(st: &State, me: usize, b: &[u8; 32], k: usize, p: Placement) -> Event {
    let scope = board::ship_scope(&st.sid, &st.seats[me]);
    Event::Placed {
        ship: k as u8,
        c: Hex(board::ship_commitment(&scope, b, k, p)),
    }
}

/// The answer duty (B§6.5 item 1). `hit` is whichever value opens our own
/// committed square, so the answer is always the opening.
fn answer(st: &State, me: usize, ctx: &Ctx, i: u16) -> Option<Event> {
    let b = board::seed(&ctx.secrets);
    let own = read_own(st, me, &b).ok()?;
    let cells = st.boards[me].cells.as_ref()?;
    let scope = board::cell_scope(&st.sid, &st.seats[me]);
    let salt = slots::salt(&b, CELL, i);
    let hit = [false, true]
        .into_iter()
        .find(|&h| slots::opens(&scope, &cells[i as usize], i, &salt, &[h as u8]))?;
    let sunk = hit
        .then(|| {
            own.iter().enumerate().find_map(|(k, p)| {
                let p = (*p)?;
                let sq = board::ship_squares(&st.rules, k, p)?;
                let done = sq.contains(&i)
                    && st.boards[me].sunk[k].is_none()
                    && sq
                        .iter()
                        .all(|&j| j == i || st.boards[me].shots[j as usize] == Some(true));
                done.then(|| Sunk {
                    ship: k as u8,
                    salt: Hex(slots::salt(&b, SHIP, k as u16)),
                    at: p,
                })
            })
        })
        .flatten();
    Some(Event::Answer {
        cell: i,
        hit,
        salt: Hex(salt),
        sunk,
    })
}

fn reveal(st: &State, me: usize, ctx: &Ctx) -> Option<Event> {
    let b = board::seed(&ctx.secrets);
    let fleet: Option<Vec<Placement>> = read_own(st, me, &b).ok()?.into_iter().collect();
    Some(Event::Reveal {
        b: Hex(b),
        fleet: fleet?,
    })
}

impl Game for Battleship {
    const ID: &'static str = "battleship";
    const DEFAULT_RULESET: &'static str = "classic";
    const FORMATS: &'static [&'static str] = &["bridge", "ledger", "logbook"];
    type Rules = RuleSet;
    type State = State;
    type Move = Move;
    type Event = Event;

    fn seats() -> std::ops::RangeInclusive<usize> {
        2..=2
    }

    fn default_format(tty: bool, no_color: bool) -> &'static str {
        if tty && !no_color {
            "bridge"
        } else {
            "logbook"
        }
    }

    fn ruleset(name: &str) -> Result<RuleSet, String> {
        RuleSet::by_name(name)
    }

    fn rules_name(rules: &RuleSet) -> &str {
        &rules.name
    }

    fn describe(rules: &RuleSet) -> String {
        let lens: Vec<String> = rules.fleet.iter().map(|s| s.len.to_string()).collect();
        format!(
            "Battleship, ruleset {} ({}×{}, fleet {}{})",
            rules.name,
            rules.rows,
            rules.cols,
            lens.join(" "),
            if rules.touching {
                ""
            } else {
                ", ships may not touch"
            }
        )
    }

    fn initial(rules: RuleSet, info: &SessionInfo) -> State {
        let n = rules.fleet.len();
        let board = Board {
            placed: vec![None; n],
            cells: None,
            shots: vec![None; rules.cells() as usize],
            sunk: vec![None; n],
            hits: 0,
            misses: 0,
        };
        State {
            sid: info.session_id.clone(),
            seats: info.seats.clone(),
            phase: Phase::Setup,
            boards: vec![board; info.seats.len()],
            turn: 0,
            pending: None,
            history: Vec::new(),
            provisional: None,
            outcome: None,
            rules,
        }
    }

    /// Every verb starts a move; after `place`, each further triple is its
    /// own move; a bare square fires.
    fn parse_moves(tokens: &[String]) -> Result<Vec<Move>, String> {
        let mut moves = Vec::new();
        let mut rest = tokens;
        while let Some((token, tail)) = rest.split_first() {
            rest = tail;
            if board::looks_like_square(token) {
                moves.push(Move::Fire(token.clone()));
                continue;
            }
            match as_verb(token) {
                Some(Verb::Ready) => moves.push(Move::Ready),
                Some(Verb::Fire) => {
                    let (sq, tail) = rest
                        .split_first()
                        .ok_or("fire takes a square, e.g. `fire B5`")?;
                    rest = tail;
                    moves.push(Move::Fire(sq.clone()));
                }
                Some(Verb::Place) => {
                    if rest.first().is_some_and(|t| t.eq_ignore_ascii_case("auto")) {
                        rest = &rest[1..];
                        moves.push(Move::PlaceAuto);
                        continue;
                    }
                    let mut any = false;
                    while let Some(t) = rest.first() {
                        if as_verb(t).is_some() || board::looks_like_square(t) {
                            break;
                        }
                        let [ship, square, dir, ..] = rest else {
                            return Err(PLACE_USAGE.into());
                        };
                        let dir = parse_dir(dir).ok_or(PLACE_USAGE)?;
                        moves.push(Move::Place {
                            ship: ship.clone(),
                            square: square.clone(),
                            dir,
                        });
                        rest = &rest[3..];
                        any = true;
                    }
                    if !any {
                        return Err(PLACE_USAGE.into());
                    }
                }
                None => return Err(format!("unknown move `{}` ({MOVES_HINT})", escape(token))),
            }
        }
        if moves.is_empty() {
            return Err(format!("no move given ({MOVES_HINT})"));
        }
        Ok(moves)
    }

    fn fold(st: &mut State, a: usize, ev: &Event) -> Result<(), String> {
        if st.phase == Phase::Over {
            return Err("the game is over".into());
        }
        let seat = st.seats[a].clone();
        let n = st.rules.fleet.len();
        match ev {
            Event::Placed { ship, c } => {
                if st.locked(a) {
                    return Err(format!("{seat} moved a ship after locking the fleet"));
                }
                let k = *ship as usize;
                if k >= n {
                    return Err(format!("no ship #{ship} in this fleet"));
                }
                st.boards[a].placed[k] = Some(c.0);
            }
            Event::Fleet { cells } => {
                if st.locked(a) {
                    return Err(format!("{seat} locked the fleet twice"));
                }
                if let Some(k) = st.boards[a].placed.iter().position(Option::is_none) {
                    return Err(format!(
                        "{seat} locked the fleet before placing the {}",
                        st.ship(k)
                    ));
                }
                if cells.len() != st.rules.cells() as usize {
                    return Err(format!(
                        "{seat}'s fleet has {} square commitments; the board has {}",
                        cells.len(),
                        st.rules.cells()
                    ));
                }
                st.boards[a].cells = Some(cells.iter().map(|c| c.0).collect());
                if st.unlocked().is_empty() {
                    st.phase = Phase::Battle;
                    st.turn = 0;
                }
            }
            Event::Shot { cell } => {
                st.battle_only()?;
                if a != st.turn {
                    return Err(format!("{seat} fired out of turn"));
                }
                if st.pending.is_some() {
                    return Err(format!(
                        "{seat} fired again before {} answered",
                        st.seats[1 - a]
                    ));
                }
                if *cell >= st.rules.cells() {
                    return Err(format!("square {cell} isn't on the board"));
                }
                if st.boards[1 - a].shots[*cell as usize].is_some() {
                    return Err(format!("{seat} already fired at {}", st.sq(*cell)));
                }
                st.pending = Some(*cell);
                st.history.push(ShotRecord {
                    by: a,
                    cell: *cell,
                    hit: None,
                    sunk: None,
                });
            }
            Event::Answer {
                cell,
                hit,
                salt,
                sunk,
            } => {
                st.battle_only()?;
                let Some(i) = st.pending else {
                    return Err("no shot is waiting for an answer".into());
                };
                let d = 1 - st.turn;
                if a != d {
                    return Err(format!(
                        "only {} can answer the shot at {}",
                        st.seats[d],
                        st.sq(i)
                    ));
                }
                if *cell != i {
                    return Err(format!(
                        "{seat} answered {} but the shot was at {}",
                        st.sq(*cell),
                        st.sq(i)
                    ));
                }
                let cells = st.boards[a].cells.as_ref().expect("Battle implies locked");
                let scope = board::cell_scope(&st.sid, &seat);
                if !slots::opens(&scope, &cells[i as usize], i, &salt.0, &[*hit as u8]) {
                    return Err(format!(
                        "{seat}'s answer for {} doesn't open {seat}'s commitment for that square",
                        st.sq(i)
                    ));
                }
                if let Some(s) = sunk {
                    let k = s.ship as usize;
                    if k >= n {
                        return Err(format!("no ship #{} in this fleet", s.ship));
                    }
                    if st.boards[a].sunk[k].is_some() {
                        return Err(format!("{seat}'s {} is already sunk", st.ship(k)));
                    }
                    let sk = st.boards[a].placed[k].expect("locked implies placed");
                    let scope = board::ship_scope(&st.sid, &seat);
                    if !slots::opens(&scope, &sk, k as u16, &s.salt.0, &s.at.enc()) {
                        return Err(format!(
                            "{seat}'s sunk {} doesn't open {seat}'s {} commitment",
                            st.ship(k),
                            st.ship(k)
                        ));
                    }
                }
                // B§6.3, in order.
                let b = &mut st.boards[a];
                b.shots[i as usize] = Some(*hit);
                if *hit {
                    b.hits += 1;
                } else {
                    b.misses += 1;
                }
                st.history
                    .last_mut()
                    .expect("a pending shot is recorded")
                    .hit = Some(*hit);
                if let Some(s) = sunk {
                    let k = s.ship as usize;
                    if let Some(cheat) = sunk_cheat(&st.rules, &st.boards[a], k, s.at, i) {
                        st.forfeit(a, cheat);
                        return Ok(());
                    }
                    st.boards[a].sunk[k] = Some(s.at);
                    st.history.last_mut().expect("recorded").sunk = Some(s.ship);
                }
                let b = &st.boards[a];
                if b.hits == st.rules.fleet_cells() {
                    let declared = b.sunk.iter().filter(|s| s.is_some()).count();
                    if declared == n {
                        st.provisional = Some(st.turn);
                        st.phase = Phase::Reveal;
                        st.pending = None;
                    } else {
                        st.forfeit(
                            a,
                            Cheat::UnsunkAtFull {
                                sunk: declared as u8,
                            },
                        );
                    }
                } else if b.misses > st.rules.cells() - st.rules.fleet_cells() {
                    let misses = b.misses;
                    st.forfeit(a, Cheat::TooManyMisses { misses });
                } else {
                    st.pending = None;
                    st.turn = a;
                }
            }
            Event::Reveal { b, fleet } => {
                let Some(w) = st.provisional.filter(|_| st.phase == Phase::Reveal) else {
                    return Err("reveal before the battle is over".into());
                };
                if a != w {
                    return Err(format!("{seat} owes no reveal: {seat}'s fleet was sunk"));
                }
                if fleet.len() != n {
                    return Err(format!(
                        "{seat}'s reveal lists {} ships; the fleet has {n}",
                        fleet.len()
                    ));
                }
                let cells = st.boards[a].cells.as_ref().expect("Reveal implies locked");
                let scope = board::cell_scope(&st.sid, &seat);
                let mut occ = Vec::with_capacity(cells.len());
                for (i, c) in cells.iter().enumerate() {
                    let i = i as u16;
                    let salt = slots::salt(&b.0, CELL, i);
                    let o = [0u8, 1]
                        .into_iter()
                        .find(|&o| slots::opens(&scope, c, i, &salt, &[o]))
                        .ok_or_else(|| {
                            format!(
                                "{seat}'s board secret doesn't open {seat}'s commitment for {}",
                                st.sq(i)
                            )
                        })?;
                    occ.push(o);
                }
                let scope = board::ship_scope(&st.sid, &seat);
                for (k, p) in fleet.iter().enumerate() {
                    let sk = st.boards[a].placed[k].expect("locked implies placed");
                    if board::ship_commitment(&scope, &b.0, k, *p) != sk {
                        return Err(format!(
                            "{seat}'s revealed {} doesn't open {seat}'s {} commitment",
                            st.ship(k),
                            st.ship(k)
                        ));
                    }
                }
                if let Err(cheat) = board::legality(&st.rules, fleet, &occ) {
                    st.forfeit(a, cheat);
                    return Ok(());
                }
                let mine = &st.boards[a];
                let withheld = fleet.iter().enumerate().position(|(k, p)| {
                    mine.sunk[k].is_none()
                        && board::ship_squares(&st.rules, k, *p)
                            .expect("legal implies on the board")
                            .iter()
                            .all(|&j| mine.shots[j as usize] == Some(true))
                });
                if let Some(k) = withheld {
                    st.forfeit(a, Cheat::Withheld { ship: k as u8 });
                    return Ok(());
                }
                st.outcome = Some(Outcome {
                    winner: w,
                    how: How::Sank,
                });
                st.phase = Phase::Over;
            }
        }
        Ok(())
    }

    fn plan(st: &State, me: usize, mv: &Move, ctx: &mut Ctx) -> Result<Vec<Event>, String> {
        let rules = &st.rules;
        let b = board::seed(&ctx.secrets);
        match mv {
            Move::Place { ship, square, dir } => {
                if st.locked(me) {
                    return Err("your fleet is locked: ships can't move after ready".into());
                }
                let k = resolve_ship(rules, ship)?;
                let (row, col) = board::parse_square(rules, square)?;
                let p = Placement {
                    row,
                    col,
                    dir: *dir,
                };
                let cur = read_own(st, me, &b)?;
                board::check_place(rules, &cur, k, p)?;
                Ok(vec![placed_event(st, me, &b, k, p)])
            }
            Move::PlaceAuto => {
                if st.locked(me) {
                    return Err("your fleet is locked: ships can't move after ready".into());
                }
                let cur = read_own(st, me, &b)?;
                let fleet = board::auto(rules, &cur, &mut ctx.rng)?;
                let reroll = cur.iter().all(Option::is_some);
                Ok(fleet
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| reroll || cur[*k].is_none())
                    .map(|(k, p)| placed_event(st, me, &b, k, *p))
                    .collect())
            }
            Move::Ready => {
                if st.locked(me) {
                    return Err("your fleet is locked: ships can't move after ready".into());
                }
                let cur = read_own(st, me, &b)?;
                let missing: Vec<&str> = (0..cur.len())
                    .filter(|&k| cur[k].is_none())
                    .map(|k| st.ship(k))
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "place the {} first (or place auto)",
                        list(&missing)
                    ));
                }
                let fleet: Vec<Placement> = cur.into_iter().flatten().collect();
                let occ = board::occupancy(rules, &fleet);
                let values: Vec<&[u8]> = occ.chunks(1).collect();
                let scope = board::cell_scope(&st.sid, &st.seats[me]);
                let cells = slots::commit_all(&scope, &b, &values);
                Ok(vec![Event::Fleet {
                    cells: cells.into_iter().map(Hex).collect(),
                }])
            }
            Move::Fire(square) => {
                let (row, col) = board::parse_square(rules, square)?;
                let i = row as u16 * rules.cols as u16 + col as u16;
                if st.boards[1 - me].shots[i as usize].is_some() {
                    return Err(format!("you already fired at {}", st.sq(i)));
                }
                Ok(vec![Event::Shot { cell: i }])
            }
        }
    }

    fn duty(st: &State, me: usize, ctx: &Ctx) -> Option<Event> {
        match st.phase {
            Phase::Battle if st.turn != me => answer(st, me, ctx, st.pending?),
            Phase::Reveal if st.provisional == Some(me) => reveal(st, me, ctx),
            _ => None,
        }
    }

    fn to_move(st: &State) -> Option<usize> {
        (st.phase == Phase::Battle && st.pending.is_none()).then_some(st.turn)
    }

    fn may_move(st: &State, me: usize, names: &[String]) -> Result<(), String> {
        if st.phase == Phase::Setup {
            if !st.locked(me) {
                return Ok(());
            }
            let others: Vec<&str> = st.unlocked().iter().map(|&s| names[s].as_str()).collect();
            return Err(format!(
                "your fleet is locked; waiting on {} to place theirs",
                list(&others)
            ));
        }
        match Self::to_move(st) {
            Some(p) if p == me => Ok(()),
            Some(o) => Err(format!("it's {}'s turn, not yours", names[o])),
            None => Err(format!("waiting on {}", Self::waiting(st, names))),
        }
    }

    fn winner(st: &State) -> Option<usize> {
        st.outcome.map(|o| o.winner)
    }

    fn waiting(st: &State, names: &[String]) -> String {
        match st.phase {
            Phase::Setup => {
                let who: Vec<&str> = st.unlocked().iter().map(|&s| names[s].as_str()).collect();
                format!("{} to place their fleet", list(&who))
            }
            Phase::Battle => match st.pending {
                Some(i) => format!(
                    "{}'s answer to the shot at {}",
                    names[1 - st.turn],
                    st.sq(i)
                ),
                None => format!("{}'s shot", names[st.turn]),
            },
            Phase::Reveal => format!(
                "{}'s final reveal",
                names[st.provisional.expect("Reveal has a provisional winner")]
            ),
            Phase::Over => "nobody: the game is over".into(),
        }
    }

    fn narrate(
        before: &State,
        author: usize,
        ev: &Event,
        after: &State,
        names: &[String],
    ) -> Vec<String> {
        let name = &names[author];
        let mut out = Vec::new();
        match ev {
            Event::Placed { ship, .. } => {
                out.push(format!("{name} placed the {}", after.ship(*ship as usize)))
            }
            Event::Fleet { .. } => {
                out.push(format!("{name} locked the fleet"));
                if after.phase == Phase::Battle {
                    out.push(format!("Both fleets locked. {} fires first.", names[0]));
                }
            }
            Event::Shot { cell } => out.push(format!("{name} fires at {}", after.sq(*cell))),
            Event::Answer { cell, hit, .. } => {
                let attacker = &names[before.turn];
                let mut line = format!(
                    "{name}: {} — {}",
                    after.sq(*cell),
                    if *hit { "hit" } else { "miss" }
                );
                if let Some(k) = after.history.last().and_then(|r| r.sunk) {
                    line.push_str(&format!(
                        ", {attacker} sank {name}'s {}",
                        after.ship(k as usize)
                    ));
                }
                out.push(line);
                if after.phase == Phase::Reveal {
                    out.push(format!(
                        "{attacker} sank {name}'s fleet; waiting on {attacker}'s final reveal to confirm"
                    ));
                }
                out.extend(cheat_line(after, names));
            }
            Event::Reveal { fleet, .. } => {
                let ships: Vec<String> = fleet
                    .iter()
                    .enumerate()
                    .map(|(k, p)| format!("{} {}", after.ship(k), p.short()))
                    .collect();
                let mut line = format!("{name} reveals: {}", ships.join(", "));
                if let Some(Outcome { how: How::Sank, .. }) = after.outcome {
                    line.push_str(" — legal, every answer matched");
                }
                out.push(line);
                match after.outcome {
                    Some(Outcome { how: How::Sank, .. }) => {
                        out.extend(Self::game_over(after, names));
                    }
                    _ => out.extend(cheat_line(after, names)),
                }
            }
        }
        out
    }

    fn recap(before: &State, ev: &Event, _names: &[String]) -> Option<String> {
        match ev {
            Event::Placed { ship, .. } => before
                .rules
                .fleet
                .get(*ship as usize)
                .map(|s| format!("placed the {}", s.name)),
            Event::Fleet { .. } => Some("locked the fleet".into()),
            Event::Shot { cell } => Some(format!("fired at {}", before.sq(*cell))),
            Event::Answer { .. } | Event::Reveal { .. } => None,
        }
    }

    fn moment(before: &State) -> Option<String> {
        (before.phase == Phase::Battle).then(|| format!("shot {}", before.history.len() + 1))
    }

    fn game_over(st: &State, names: &[String]) -> Option<String> {
        let o = st.outcome?;
        Some(match o.how {
            How::Sank => format!(
                "{w} won: sank {}'s fleet; {w}'s fleet verified",
                names[1 - o.winner],
                w = names[o.winner]
            ),
            How::Forfeit { liar, cheat } => format!(
                "{} won: {} forfeits — {}",
                names[o.winner],
                names[liar],
                words_for(st, liar, &cheat, names)
            ),
        })
    }

    fn render(st: &State, names: &[String], me: Option<usize>, ctx: Option<&Ctx>) -> String {
        let logbook = Format {
            id: "logbook",
            color: false,
        };
        Self::render_as(st, names, me, ctx, logbook)
    }

    fn render_as(
        st: &State,
        names: &[String],
        me: Option<usize>,
        ctx: Option<&Ctx>,
        format: Format,
    ) -> String {
        let v = view(st, me, ctx);
        match format.id {
            "bridge" => draw::draw_bridge(&v, names, format.color),
            "ledger" => draw::draw_ledger(&v, names),
            _ => draw::draw_logbook(&v, names),
        }
    }
}

/// `CHEAT: <words>. <liar> forfeits; <other> wins.` once a forfeit decided.
fn cheat_line(st: &State, names: &[String]) -> Option<String> {
    let o = st.outcome?;
    let How::Forfeit { liar, cheat } = o.how else {
        return None;
    };
    Some(format!(
        "CHEAT: {}. {} forfeits; {} wins.",
        words_for(st, liar, &cheat, names),
        names[liar],
        names[o.winner]
    ))
}

/// B§9: everything one seat may know, and nothing else. Only `own` needs
/// the password.
pub fn view(st: &State, me: Option<usize>, ctx: Option<&Ctx>) -> View {
    let mine = me.unwrap_or(0);
    let opp = 1 - mine;
    let rules = &st.rules;
    let cells = rules.cells();
    let mut error = None;
    let own_placed = match (me, ctx) {
        (Some(me), Some(ctx)) => match read_own(st, me, &board::seed(&ctx.secrets)) {
            Ok(p) => Some(p),
            Err(e) => {
                error = Some(e);
                None
            }
        },
        _ => None,
    };
    let pending_at = |seat: usize| st.pending.filter(|_| st.turn != seat);
    let part_map = |placed: &[Option<Placement>]| {
        let mut parts = vec![None; cells as usize];
        for (k, p) in placed.iter().enumerate() {
            let Some(p) = p else { continue };
            for (t, i) in board::ship_squares(rules, k, *p)
                .into_iter()
                .flatten()
                .enumerate()
            {
                parts[i as usize] = Some(ShipPart {
                    ship: k as u8,
                    part: t as u8,
                    dir: p.dir,
                });
            }
        }
        parts
    };
    let own = own_placed.map(|placed| {
        let parts = part_map(&placed);
        let b = &st.boards[mine];
        let squares = (0..cells)
            .map(|i| OwnSquare {
                ship: parts[i as usize],
                incoming: match b.shots[i as usize] {
                    Some(true) => Incoming::Hit,
                    Some(false) => Incoming::Miss,
                    None if pending_at(mine) == Some(i) => Incoming::Pending,
                    None => Incoming::None,
                },
            })
            .collect();
        OwnView { squares, placed }
    });
    let theirs = &st.boards[opp];
    let sunk_parts = part_map(&theirs.sunk);
    let target = (0..cells)
        .map(|i| match theirs.shots[i as usize] {
            Some(false) => TargetSquare::Miss,
            Some(true) => match sunk_parts[i as usize] {
                Some(part) => TargetSquare::Sunk(part),
                None => TargetSquare::Hit,
            },
            None if pending_at(opp) == Some(i) => TargetSquare::Pending,
            None => TargetSquare::Unknown,
        })
        .collect();
    let status = |seat: usize| {
        let b = &st.boards[seat];
        SideStatus {
            sunk: b.sunk.clone(),
            hits: b.hits,
            misses: b.misses,
            locked: b.cells.is_some(),
        }
    };
    let last = |by: usize| {
        st.history
            .iter()
            .rev()
            .find(|r| r.by == by)
            .map(|r| ShotView {
                cell: r.cell,
                result: r.hit.map(|h| (h, r.sunk)),
            })
    };
    let phase = match st.phase {
        Phase::Setup => ViewPhase::Placing {
            locked: st.locked(mine),
            opponent_placed: theirs.placed.iter().filter(|c| c.is_some()).count() as u8,
            opponent_locked: st.locked(opp),
            next: own
                .as_ref()
                .and_then(|o| o.placed.iter().position(Option::is_none))
                .map(|k| k as u8),
        },
        Phase::Battle => ViewPhase::Battle {
            my_turn: st.turn == mine && st.pending.is_none(),
        },
        Phase::Reveal => {
            let w = st.provisional.expect("Reveal has a provisional winner");
            ViewPhase::Reveal {
                provisional: w,
                waiting_on: vec![w],
            }
        }
        Phase::Over => {
            let o = st.outcome.expect("Over has an outcome");
            ViewPhase::Over {
                winner: o.winner,
                how: match o.how {
                    How::Sank => HowView::Sank,
                    How::Forfeit { liar, cheat } => HowView::Forfeit { liar, cheat },
                },
            }
        }
    };
    View {
        rows: rules.rows,
        cols: rules.cols,
        fleet: fleet_names(rules),
        ruleset: rules.name.clone(),
        touching: rules.touching,
        me,
        phase,
        own,
        target,
        fleets: [status(mine), status(opp)],
        last_mine: last(mine),
        last_theirs: last(opp),
        error,
    }
}

#[cfg(test)]
mod tests;
