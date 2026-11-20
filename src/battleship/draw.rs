//! What a seat may see (B§9's `View`) and the three looks that draw it
//! (render formats R1-R4). Every `draw_*` is a pure function of a `View` and
//! the names: it can't show more than the seat is entitled to, because the
//! `View` is all it gets.

use super::board::{self, Dir, Placement};
use super::{cheat_words, Cheat};

/// Seat indices inside (`me`, `provisional`, `winner`, `liar`) are absolute;
/// `fleets` is `[me, opponent]`.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub rows: u8,
    pub cols: u8,
    /// (name, len) in fleet order.
    pub fleet: Vec<(String, u8)>,
    pub ruleset: String,
    pub touching: bool,
    /// None = no password.
    pub me: Option<usize>,
    pub phase: ViewPhase,
    /// My ships, read back from the log; needs the password.
    pub own: Option<OwnView>,
    /// My shots at the opponent, rows*cols, row-major.
    pub target: Vec<TargetSquare>,
    /// Public per-seat ship status.
    pub fleets: [SideStatus; 2],
    pub last_mine: Option<ShotView>,
    pub last_theirs: Option<ShotView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewPhase {
    Placing {
        locked: bool,
        opponent_placed: u8,
        opponent_locked: bool,
        /// The first unplaced ship.
        next: Option<u8>,
    },
    Battle {
        my_turn: bool,
    },
    Reveal {
        provisional: usize,
        waiting_on: Vec<usize>,
    },
    Over {
        winner: usize,
        how: HowView,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HowView {
    Sank,
    Forfeit { liar: usize, cheat: Cheat },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnView {
    pub squares: Vec<OwnSquare>,
    pub placed: Vec<Option<Placement>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OwnSquare {
    pub ship: Option<ShipPart>,
    pub incoming: Incoming,
}

/// part 0 = bow (top/left) … len-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShipPart {
    pub ship: u8,
    pub part: u8,
    pub dir: Dir,
}

/// The opponent's shots at me. Sunk is the ship's entry in `fleets[0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incoming {
    None,
    Pending,
    Miss,
    Hit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSquare {
    Unknown,
    Pending,
    Miss,
    Hit,
    Sunk(ShipPart),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SideStatus {
    pub sunk: Vec<Option<Placement>>,
    pub hits: u16,
    pub misses: u16,
    pub locked: bool,
}

/// (hit, sunk ship) once answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShotView {
    pub cell: u16,
    pub result: Option<(bool, Option<u8>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Water,
    Ship,
    Hit,
    Sunk,
    Miss,
    Pending,
}

impl View {
    fn me(&self) -> usize {
        self.me.unwrap_or(0)
    }

    fn cells(&self) -> u16 {
        self.rows as u16 * self.cols as u16
    }

    fn sq(&self, i: u16) -> String {
        board::sq_name(self.cols, i)
    }

    fn squares_of(&self, k: usize, p: Placement) -> Vec<u16> {
        board::squares(self.rows, self.cols, p, self.fleet[k].1).unwrap_or_default()
    }

    /// `A2-A6`, origin to end.
    fn span(&self, k: usize, p: Placement) -> String {
        match self.squares_of(k, p)[..] {
            [first, .., last] => format!("{}-{}", self.sq(first), self.sq(last)),
            [one] => self.sq(one),
            [] => p.short(),
        }
    }

    fn own_mark(&self, i: u16) -> Mark {
        let Some(own) = &self.own else {
            return Mark::Water;
        };
        let s = own.squares[i as usize];
        match (s.ship, s.incoming) {
            (Some(p), _) if self.fleets[0].sunk[p.ship as usize].is_some() => Mark::Sunk,
            (_, Incoming::Hit) => Mark::Hit,
            (_, Incoming::Pending) => Mark::Pending,
            (Some(_), _) => Mark::Ship,
            (None, Incoming::Miss) => Mark::Miss,
            (None, Incoming::None) => Mark::Water,
        }
    }

    fn target_mark(&self, i: u16) -> Mark {
        match self.target[i as usize] {
            TargetSquare::Unknown => Mark::Water,
            TargetSquare::Pending => Mark::Pending,
            TargetSquare::Miss => Mark::Miss,
            TargetSquare::Hit => Mark::Hit,
            TargetSquare::Sunk(_) => Mark::Sunk,
        }
    }

    fn afloat(&self, side: usize) -> usize {
        self.fleets[side]
            .sunk
            .iter()
            .filter(|s| s.is_none())
            .count()
    }

    /// My hits on squares that aren't part of a sunk ship.
    fn open_hits(&self) -> Vec<u16> {
        (0..self.cells())
            .filter(|&i| self.target[i as usize] == TargetSquare::Hit)
            .collect()
    }

    /// Whether some afloat enemy ship could lie across `i`, given my misses,
    /// the sunk ships (and, under notouch, their neighbours) and the edges.
    fn coverable(&self, i: u16) -> bool {
        let sunk: Vec<Vec<u16>> = self.fleets[1]
            .sunk
            .iter()
            .enumerate()
            .filter_map(|(k, p)| p.map(|p| self.squares_of(k, p)))
            .collect();
        let (r, c) = ((i / self.cols as u16) as u8, (i % self.cols as u16) as u8);
        let fits = |len: u8, p: Placement| {
            board::squares(self.rows, self.cols, p, len).is_some_and(|sq| {
                sq.iter().all(|&j| {
                    matches!(
                        self.target[j as usize],
                        TargetSquare::Unknown | TargetSquare::Pending | TargetSquare::Hit
                    )
                }) && (self.touching || sunk.iter().all(|o| !board::adjacent(self.cols, &sq, o)))
            })
        };
        (0..self.fleet.len())
            .filter(|&k| self.fleets[1].sunk[k].is_none())
            .any(|k| {
                let len = self.fleet[k].1;
                (0..len).any(|t| {
                    let across = c.checked_sub(t).map(|col| Placement {
                        row: r,
                        col,
                        dir: Dir::H,
                    });
                    let down = r.checked_sub(t).map(|row| Placement {
                        row,
                        col: c,
                        dir: Dir::V,
                    });
                    [across, down].into_iter().flatten().any(|p| fits(len, p))
                })
            })
    }

    /// An unshot square for the `e.g.`: past either end of a line of open
    /// hits, else next to the latest open hit, else the first square a ship
    /// could still be on. Never one no afloat ship could cover.
    pub(super) fn suggestion(&self) -> Option<u16> {
        let (rows, cols) = (self.rows as i32, self.cols as i32);
        let at = |r: i32, c: i32| {
            ((0..rows).contains(&r) && (0..cols).contains(&c)).then(|| (r * cols + c) as u16)
        };
        let hit = |r, c| at(r, c).is_some_and(|i| self.target[i as usize] == TargetSquare::Hit);
        let open = |r, c| {
            at(r, c)
                .filter(|&i| self.target[i as usize] == TargetSquare::Unknown && self.coverable(i))
        };
        let mut hits = self.open_hits();
        hits.reverse();
        if let Some(i) = self.last_mine.map(|s| s.cell).filter(|i| hits.contains(i)) {
            hits.retain(|&j| j != i);
            hits.insert(0, i);
        }
        for &i in &hits {
            let (r, c) = (i as i32 / cols, i as i32 % cols);
            for (dr, dc) in [(0, 1), (1, 0)] {
                let run = |sign: i32| {
                    (1..)
                        .take_while(|&n| hit(r + sign * n * dr, c + sign * n * dc))
                        .count() as i32
                };
                let (hi, lo) = (run(1), run(-1));
                if hi + lo == 0 {
                    continue;
                }
                for n in [hi + 1, -(lo + 1)] {
                    if let Some(j) = open(r + n * dr, c + n * dc) {
                        return Some(j);
                    }
                }
            }
        }
        for &i in &hits {
            let (r, c) = (i as i32 / cols, i as i32 % cols);
            for (dr, dc) in [(0, 1), (0, -1), (1, 0), (-1, 0)] {
                if let Some(j) = open(r + dr, c + dc) {
                    return Some(j);
                }
            }
        }
        (0..self.cells())
            .find(|&i| self.target[i as usize] == TargetSquare::Unknown && self.coverable(i))
    }

    /// The first legal spot for ship `k` among my placed ships, for the `e.g.`.
    fn example_spot(&self, k: usize) -> Option<Placement> {
        let own = self.own.as_ref()?;
        let others: Vec<Vec<u16>> = own
            .placed
            .iter()
            .enumerate()
            .filter(|(m, _)| *m != k)
            .filter_map(|(m, p)| p.map(|p| self.squares_of(m, p)))
            .collect();
        for row in 0..self.rows {
            for col in 0..self.cols {
                for dir in [Dir::H, Dir::V] {
                    let p = Placement { row, col, dir };
                    let Some(sq) = board::squares(self.rows, self.cols, p, self.fleet[k].1) else {
                        continue;
                    };
                    let clear = others.iter().all(|o| {
                        board::first_shared(&sq, o).is_none()
                            && (self.touching || !board::adjacent(self.cols, &sq, o))
                    });
                    if clear {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    fn incoming_hits(&self, k: usize, p: Placement) -> Vec<u16> {
        let Some(own) = &self.own else {
            return Vec::new();
        };
        self.squares_of(k, p)
            .into_iter()
            .filter(|&i| own.squares[i as usize].incoming == Incoming::Hit)
            .collect()
    }

    fn shot_number(&self) -> u16 {
        self.fleets.iter().map(|f| f.hits + f.misses).sum::<u16>() + 1
    }

    fn words(&self, cheat: &Cheat, subj: &str, poss: &str) -> String {
        cheat_words(cheat, self.cols, &self.fleet, &self.ruleset, subj, poss)
    }
}

/// Who a line is about, in one look's words.
struct Voice<'a> {
    me: &'a str,
    my: &'a str,
    them: String,
    their: String,
}

impl Voice<'_> {
    fn of(&self, v: &View, seat: usize) -> (&str, &str) {
        if seat == v.me() {
            (self.me, self.my)
        } else {
            (&self.them, &self.their)
        }
    }
}

/// The end-of-game sentence in `voice`: `you won: sank Bob's fleet; your fleet verified`.
fn ending(v: &View, voice: &Voice) -> Option<String> {
    Some(match &v.phase {
        ViewPhase::Reveal { provisional, .. } => {
            let (w, wp) = voice.of(v, *provisional);
            let (_, lp) = voice.of(v, 1 - provisional);
            format!("{w} sank {lp} fleet · waiting on {wp} final reveal")
        }
        ViewPhase::Over { winner, how } => {
            let (w, wp) = voice.of(v, *winner);
            let (_, lp) = voice.of(v, 1 - winner);
            match how {
                HowView::Sank => format!("{w} won: sank {lp} fleet; {wp} fleet verified"),
                HowView::Forfeit { liar, cheat } => {
                    let (l, lp) = voice.of(v, *liar);
                    let verb = if *liar == v.me() {
                        "forfeit"
                    } else {
                        "forfeits"
                    };
                    format!("{l} {verb}: {}", v.words(cheat, l, lp))
                }
            }
        }
        _ => return None,
    })
}

fn result_words(v: &View, s: &ShotView) -> Option<String> {
    s.result.map(|(hit, sunk)| match sunk {
        Some(k) => format!("sunk, {}", v.fleet[k as usize].0),
        None if hit => "hit".into(),
        None => "miss".into(),
    })
}

// ---------------------------------------------------------------- ledger

/// R4: one fact per line, coordinates only, never an escape. The opponent's
/// name appears only on the header line (H6); every other line says `enemy`.
pub fn draw_ledger(v: &View, names: &[String]) -> String {
    let me = v.me();
    let opp = &names[1 - me];
    let mut out = Vec::new();
    if let Some(e) = &v.error {
        out.push(format!("error: {e}"));
    }
    let list = |sq: Vec<u16>| match sq[..] {
        [] => "none".to_string(),
        _ => sq.iter().map(|&i| v.sq(i)).collect::<Vec<_>>().join(" "),
    };
    let joined = |parts: Vec<String>| match parts[..] {
        [] => "none".to_string(),
        _ => parts.join(" · "),
    };
    let placed = |k: usize| v.own.as_ref().and_then(|o| o.placed[k]);
    let voice = Voice {
        me: "you",
        my: "your",
        them: "enemy".into(),
        their: "enemy's".into(),
    };

    if let ViewPhase::Placing {
        locked,
        opponent_placed,
        opponent_locked,
        next,
    } = &v.phase
    {
        out.push(format!(
            "placing · you vs {opp} · enemy placed {opponent_placed} of {}, {}",
            v.fleet.len(),
            if *opponent_locked {
                "locked"
            } else {
                "not locked"
            }
        ));
        let fleet: Vec<String> = (0..v.fleet.len())
            .map(|k| match placed(k) {
                Some(p) => format!("{} {}", v.fleet[k].0, v.span(k, p)),
                None => format!("{} unplaced", v.fleet[k].0),
            })
            .collect();
        out.push(format!("your fleet: {}", fleet.join(" · ")));
        out.push(match (locked, next, &v.own) {
            (true, _, _) => "next: waiting on enemy to place their fleet".into(),
            (false, Some(k), _) => format!(
                "next: zs play place {} <square> <h|v>, or zs play place auto; then zs play ready",
                v.fleet[*k as usize].0
            ),
            (false, None, Some(_)) => {
                "next: zs play ready, or re-place with zs play place <ship> <square> <h|v>".into()
            }
            (false, None, None) => "next: nothing this password can do here".into(),
        });
        return out.join("\n");
    }

    out.push(match &v.phase {
        ViewPhase::Battle { my_turn } => {
            let whose = if *my_turn {
                "your shot"
            } else if v.last_mine.is_some_and(|s| s.result.is_none()) {
                "waiting on enemy's answer"
            } else {
                "enemy's shot"
            };
            format!("turn {} · you vs {opp} · {whose}", v.shot_number())
        }
        ViewPhase::Reveal { .. } => {
            format!(
                "reveal · {}",
                ending(v, &voice).expect("Reveal has an ending")
            )
        }
        _ => format!("over · {}", ending(v, &voice).expect("Over has an ending")),
    });

    let fleet: Vec<String> = (0..v.fleet.len())
        .map(|k| {
            let name = &v.fleet[k].0;
            let Some(p) = placed(k) else {
                return format!("{name} unknown");
            };
            let state = if v.fleets[0].sunk[k].is_some() {
                "SUNK".to_string()
            } else {
                match v.incoming_hits(k, p)[..] {
                    [] => "ok".to_string(),
                    ref hits => format!("hit {}", list(hits.to_vec())),
                }
            };
            format!("{name} {} {state}", v.span(k, p))
        })
        .collect();
    out.push(format!("your fleet: {}", fleet.join(" · ")));
    let enemy_misses = match &v.own {
        Some(own) => list(
            (0..v.cells())
                .filter(|&i| own.squares[i as usize].incoming == Incoming::Miss)
                .collect(),
        ),
        None => "unknown".into(),
    };
    out.push(format!("enemy shots: misses {enemy_misses}"));
    let mut shots = vec![format!("open hits {}", list(v.open_hits()))];
    let sunk: Vec<String> = v.fleets[1]
        .sunk
        .iter()
        .enumerate()
        .filter_map(|(k, p)| p.map(|p| format!("sunk {} {}", v.fleet[k].0, v.span(k, p))))
        .collect();
    if sunk.is_empty() {
        shots.push("sunk none".into());
    }
    shots.extend(sunk);
    let misses = (0..v.cells())
        .filter(|&i| v.target[i as usize] == TargetSquare::Miss)
        .collect();
    shots.push(format!("misses {}", list(misses)));
    out.push(format!("your shots: {}", shots.join(" · ")));
    let afloat = |side: usize| {
        joined(
            v.fleets[side]
                .sunk
                .iter()
                .enumerate()
                .filter(|(_, s)| s.is_none())
                .map(|(k, _)| format!("{} {}", v.fleet[k].0, v.fleet[k].1))
                .collect(),
        )
    };
    out.push(format!("enemy afloat: {}", afloat(1)));
    out.push(format!("you afloat: {}", afloat(0)));
    let last = |who: &str, s: &Option<ShotView>| match s {
        None => format!("{who} none"),
        Some(s) => {
            let r = match s.result {
                None => "pending".to_string(),
                Some((_, Some(k))) => format!("sunk {}", v.fleet[k as usize].0),
                Some((true, None)) => "hit".into(),
                Some((false, None)) => "miss".into(),
            };
            format!("{who} {} {r}", v.sq(s.cell))
        }
    };
    out.push(format!(
        "last: {} · {}",
        last("you", &v.last_mine),
        last("enemy", &v.last_theirs)
    ));
    out.push(match &v.phase {
        ViewPhase::Battle { my_turn: true } => match v.suggestion() {
            Some(i) => format!("next: zs play <square>, e.g. zs play {}", v.sq(i)),
            None => "next: zs play <square>".into(),
        },
        ViewPhase::Battle { .. } => match v.last_mine {
            Some(ShotView { cell, result: None }) => {
                format!("waiting: enemy's answer to your shot at {}", v.sq(cell))
            }
            _ => "next: waiting on enemy's shot".into(),
        },
        ViewPhase::Reveal { provisional, .. } if *provisional == me => {
            "next: zs play sends your final reveal".into()
        }
        ViewPhase::Reveal { .. } => "next: waiting on enemy's final reveal".into(),
        _ => "next: nothing, the game is over".into(),
    });
    out.join("\n")
}

// ---------------------------------------------------------------- logbook

fn logbook_glyph(m: Mark) -> char {
    match m {
        Mark::Water => '.',
        Mark::Ship => '#',
        Mark::Hit => 'X',
        Mark::Sunk => '=',
        Mark::Miss => 'o',
        Mark::Pending => '?',
    }
}

fn col_numbers(cols: u8) -> String {
    (1..=cols)
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())))
}

/// Look 1: pure ASCII, no escapes, side by side.
pub fn draw_logbook(v: &View, names: &[String]) -> String {
    let me = v.me();
    let (mine, opp) = (&names[me], &names[1 - me]);
    let row_letter = |r: u8| (b'A' + r) as char;
    let grid_row = |r: u8, mark: &dyn Fn(u16) -> Mark| {
        (0..v.cols)
            .map(|c| logbook_glyph(mark(r as u16 * v.cols as u16 + c as u16)).to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let left_w = 4 + 2 * v.cols as usize + 5;
    let mut out = Vec::new();
    if let Some(e) = &v.error {
        out.push(format!("error: {e}"));
    }
    let moment = match &v.phase {
        ViewPhase::Placing { .. } => "placing".to_string(),
        ViewPhase::Battle { .. } => format!("shot {}", v.shot_number()),
        ViewPhase::Reveal { .. } => "reveal".into(),
        ViewPhase::Over { .. } => "over".into(),
    };
    out.push(format!("--- battleship: {mine} vs {opp}, {moment} ---"));
    out.push(String::new());

    if let ViewPhase::Placing {
        locked,
        opponent_placed,
        opponent_locked,
        next,
    } = &v.phase
    {
        let mut side = vec![String::new(); v.rows as usize];
        let fleet_line = |k: usize| {
            let (name, len) = &v.fleet[k];
            let placed = v.own.as_ref().and_then(|o| o.placed[k]);
            let (tick, tail) = match placed {
                Some(p) => ("[x]", v.span(k, p) + " " + p.dir.letter()),
                None if *next == Some(k as u8) && !locked => (" > ", "next".into()),
                None => ("[ ]", String::new()),
            };
            format!("{tick} {} {len}  {tail}", pad(name, 11))
                .trim_end()
                .to_string()
        };
        side[0] = "fleet".into();
        for k in 0..v.fleet.len().min(v.rows as usize - 2) {
            side[k + 2] = fleet_line(k);
        }
        if let Some(s) = side.get_mut(v.fleet.len() + 3) {
            *s = format!(
                "{opp}: placed {opponent_placed} of {}, {}",
                v.fleet.len(),
                if *opponent_locked {
                    "locked"
                } else {
                    "not locked"
                }
            );
        }
        out.push("  your fleet".into());
        out.push(format!("    {}", col_numbers(v.cols)));
        for r in 0..v.rows {
            let left = format!("  {} {}", row_letter(r), grid_row(r, &|i| v.own_mark(i)));
            let line = format!("{}{}", pad(&left, left_w), side[r as usize]);
            out.push(line.trim_end().to_string());
        }
        out.push(String::new());
        out.push("  . water   # ship".into());
        out.extend(placing_hints(v, opp).into_iter().map(|l| format!("  {l}")));
        return out.join("\n");
    }

    out.push(format!("{}your shots", pad("  your fleet", left_w)));
    let nums = format!("    {}", col_numbers(v.cols));
    out.push(format!("{}  {}", pad(&nums, left_w), col_numbers(v.cols)));
    for r in 0..v.rows {
        let left = format!("  {} {}", row_letter(r), grid_row(r, &|i| v.own_mark(i)));
        out.push(format!(
            "{}{} {}",
            pad(&left, left_w),
            row_letter(r),
            grid_row(r, &|i| v.target_mark(i))
        ));
    }
    out.push(String::new());
    out.push("  . water   # ship   X hit   = sunk   o miss".into());
    out.push(match &v.last_mine {
        None => "  you haven't fired yet".into(),
        Some(s) => match result_words(v, s) {
            Some(r) => format!("  you fired {}: {r}", v.sq(s.cell)),
            None => format!("  you fired {}: waiting on {opp}'s answer", v.sq(s.cell)),
        },
    });
    out.push(match &v.last_theirs {
        None => format!("  {opp} hasn't fired yet"),
        Some(s) => match result_words(v, s) {
            Some(r) => format!("  {opp} fired {}: {r}", v.sq(s.cell)),
            None => format!("  {opp} fired {}: waiting on your answer", v.sq(s.cell)),
        },
    });
    let n = v.fleet.len();
    out.push(format!(
        "  afloat: you {}/{n}, {opp} {}/{n}",
        v.afloat(0),
        v.afloat(1)
    ));
    let voice = Voice {
        me: "you",
        my: "your",
        them: opp.clone(),
        their: format!("{opp}'s"),
    };
    out.push(format!(
        "  {}",
        match &v.phase {
            ViewPhase::Battle { my_turn: true } => match v.suggestion() {
                Some(i) => format!(
                    "{mine}'s turn - fire: zs play fire <square>, e.g. zs play fire {}",
                    v.sq(i)
                ),
                None => format!("{mine}'s turn - fire: zs play fire <square>"),
            },
            ViewPhase::Battle { .. } => battle_wait(v, opp),
            _ => ending(v, &voice).expect("Reveal and Over have an ending"),
        }
    ));
    out.join("\n")
}

fn battle_wait(v: &View, opp: &str) -> String {
    match v.last_mine {
        Some(ShotView { cell, result: None }) => {
            format!("waiting on {opp}'s answer to your shot at {}", v.sq(cell))
        }
        _ => format!("{opp}'s turn - waiting on {opp}'s shot"),
    }
}

/// The two placement hint lines, in the real syntax (R6).
fn placing_hints(v: &View, opp: &str) -> Vec<String> {
    let ViewPhase::Placing { locked, next, .. } = &v.phase else {
        return Vec::new();
    };
    if *locked {
        return vec![
            "your fleet is locked".into(),
            format!("firing starts once {opp} locks theirs"),
        ];
    }
    match next {
        Some(k) => {
            let name = &v.fleet[*k as usize].0;
            let eg = v
                .example_spot(*k as usize)
                .map(|p| format!(", e.g. zs play place {name} {}", p.short()))
                .unwrap_or_default();
            vec![
                format!("next: zs play place {name} <square> <h|v>{eg}"),
                "or zs play place auto for the rest; then zs play ready".into(),
            ]
        }
        None => vec![
            "next: zs play ready locks the fleet".into(),
            "or re-place: zs play place <ship> <square> <h|v>, or zs play place auto".into(),
        ],
    }
}

// ---------------------------------------------------------------- bridge

const FRAME: &str = "38;5;240";
const WATER: &str = "38;5;24";
const SHIP: &str = "38;5;250";
const MISS: &str = "38;5;117";
const HIT: &str = "1;38;5;203";
const SUNK: &str = "38;5;160";
const PENDING: &str = "1;38;5;220";
const BOLD: &str = "1";

/// The frame's fixed width (look-3), its inside, and the two board columns.
const WIDTH: usize = 78;
const INNER: usize = WIDTH - 3;
const LEFT_W: usize = 37;
const RIGHT_W: usize = INNER - LEFT_W;
const AFLOAT_AT: usize = 46;

/// Text with its visible width, so padding ignores escapes.
#[derive(Default, Clone)]
struct Span {
    s: String,
    w: usize,
}

impl Span {
    fn push(&mut self, text: &str) -> &mut Self {
        self.s.push_str(text);
        self.w += text.chars().count();
        self
    }

    fn paint(&mut self, code: &str, text: &str, color: bool) -> &mut Self {
        if color {
            self.s.push_str(&format!("\x1b[{code}m{text}\x1b[0m"));
            self.w += text.chars().count();
            self
        } else {
            self.push(text)
        }
    }

    fn pad(&mut self, w: usize) -> &mut Self {
        let n = w.saturating_sub(self.w);
        self.push(&" ".repeat(n))
    }

    fn take(&mut self) -> Span {
        std::mem::take(self)
    }
}

fn text(s: &str) -> Span {
    let mut t = Span::default();
    t.push(s);
    t
}

enum Line {
    Split(Span, Span),
    Full(Span),
    Rule,
}

fn bridge_glyph(m: Mark) -> (&'static str, &'static str) {
    match m {
        Mark::Water => ("·", WATER),
        Mark::Ship => ("■", SHIP),
        Mark::Hit => ("✕", HIT),
        Mark::Sunk => ("▓", SUNK),
        Mark::Miss => ("○", MISS),
        Mark::Pending => ("?", PENDING),
    }
}

/// Look 3: rounded box, `· ■ ✕ ▓ ○`, a roster under each board,
/// 256-color foreground. `color: false` draws the same glyphs bare.
pub fn draw_bridge(v: &View, names: &[String], color: bool) -> String {
    let me = v.me();
    let (mine, opp) = (&names[me], &names[1 - me]);
    let name_w = v.fleet.iter().map(|s| s.0.len()).max().unwrap_or(0) + 1;
    let hull_w = v.fleet.iter().map(|s| s.1 as usize).max().unwrap_or(0) + 1;
    let grid = |r: u8, mark: &dyn Fn(u16) -> Mark| {
        let mut t = Span::default();
        t.push(&format!("{}  ", (b'A' + r) as char));
        for c in 0..v.cols {
            if c > 0 {
                t.push(" ");
            }
            let (g, code) = bridge_glyph(mark(r as u16 * v.cols as u16 + c as u16));
            t.paint(code, g, color);
        }
        t
    };
    let header = || {
        let mut t = Span::default();
        t.paint(FRAME, &format!("   {}", col_numbers(v.cols)), color);
        t
    };
    let bold = |s: &str| {
        let mut t = Span::default();
        t.paint(BOLD, s, color);
        t
    };
    let hull = |t: &mut Span, marks: &[Mark]| {
        for m in marks {
            let (g, code) = bridge_glyph(*m);
            t.paint(code, g, color);
        }
        t.push(&" ".repeat(hull_w.saturating_sub(marks.len())));
    };

    let mut lines = Vec::new();
    let placing = matches!(v.phase, ViewPhase::Placing { .. });
    lines.push(Line::Split(
        bold("YOUR FLEET"),
        bold(if placing { "FLEET" } else { "YOUR SHOTS" }),
    ));
    lines.push(Line::Split(
        header(),
        if placing { Span::default() } else { header() },
    ));

    if let ViewPhase::Placing {
        locked,
        opponent_placed,
        opponent_locked,
        next,
    } = &v.phase
    {
        let mut side = vec![Span::default(); v.rows as usize];
        for (k, (name, len)) in v.fleet.iter().enumerate().take(v.rows as usize) {
            let t = &mut side[k];
            let placed = v.own.as_ref().and_then(|o| o.placed[k]);
            let marks = vec![
                if placed.is_some() {
                    Mark::Ship
                } else {
                    Mark::Water
                };
                *len as usize
            ];
            hull(t, &marks);
            t.push(&pad(name, name_w));
            match placed {
                Some(p) => {
                    t.push(&format!("{} {}", v.span(k, p), p.dir.letter()));
                }
                None if *next == Some(k as u8) && !locked => {
                    t.paint(PENDING, "next", color);
                }
                None => {}
            }
        }
        let theirs = format!(
            "{opp}: placed {opponent_placed} of {}, {}",
            v.fleet.len(),
            if *opponent_locked {
                "locked"
            } else {
                "not locked"
            }
        );
        for (t, l) in side
            .iter_mut()
            .skip(v.fleet.len() + 1)
            .zip(wrap(&theirs, RIGHT_W - 1))
        {
            t.push(&l);
        }
        for r in 0..v.rows {
            lines.push(Line::Split(
                grid(r, &|i| v.own_mark(i)),
                side[r as usize].take(),
            ));
        }
        lines.push(Line::Rule);
        for h in placing_hints(v, opp) {
            full(&mut lines, &format!("▸ {h}"));
        }
        lines.push(Line::Full(legend(color, false)));
    } else {
        for r in 0..v.rows {
            lines.push(Line::Split(
                grid(r, &|i| v.own_mark(i)),
                grid(r, &|i| v.target_mark(i)),
            ));
        }
        lines.push(Line::Split(Span::default(), Span::default()));
        for (k, (name, len)) in v.fleet.iter().enumerate() {
            let mut left = Span::default();
            let placed = v.own.as_ref().and_then(|o| o.placed[k]);
            let mine_sunk = v.fleets[0].sunk[k].is_some();
            let marks: Vec<Mark> = match placed {
                Some(p) => v
                    .squares_of(k, p)
                    .iter()
                    .map(|&i| match v.own_mark(i) {
                        Mark::Pending => Mark::Ship,
                        m => m,
                    })
                    .collect(),
                None => vec![Mark::Water; *len as usize],
            };
            hull(&mut left, &marks);
            left.push(&pad(name, name_w));
            let hits = placed.map_or(0, |p| v.incoming_hits(k, p).len());
            if mine_sunk {
                left.paint(SUNK, "sunk", color);
            } else if hits > 0 {
                let s = if hits == 1 { "" } else { "s" };
                left.paint(HIT, &format!("{hits} hit{s}"), color);
            }
            let mut right = Span::default();
            let theirs_sunk = v.fleets[1].sunk[k].is_some();
            let mark = if theirs_sunk { Mark::Sunk } else { Mark::Ship };
            hull(&mut right, &vec![mark; *len as usize]);
            right.push(&pad(name, name_w));
            if theirs_sunk {
                right.paint(SUNK, "sunk", color);
            }
            lines.push(Line::Split(left, right));
        }
        lines.push(Line::Rule);
        let mine_line = match &v.last_mine {
            None => "▸ you haven't fired yet".to_string(),
            Some(s) => match result_words(v, s) {
                Some(r) => format!("▸ you fired {} — {r}", v.sq(s.cell)),
                None => format!("▸ you fired {} — waiting on {opp}", v.sq(s.cell)),
            },
        };
        let theirs_line = match &v.last_theirs {
            None => format!("◂ {opp} hasn't fired yet"),
            Some(s) => match result_words(v, s) {
                Some(r) => format!("◂ {opp} fired {} — {r}", v.sq(s.cell)),
                None => format!("◂ {opp} fired {} — waiting on you", v.sq(s.cell)),
            },
        };
        if mine_line.chars().count() + 2 <= LEFT_W && theirs_line.chars().count() < RIGHT_W {
            lines.push(Line::Split(text(&mine_line), text(&theirs_line)));
        } else {
            full(&mut lines, &mine_line);
            full(&mut lines, &theirs_line);
        }
        let n = v.fleet.len();
        let afloat = format!(
            "afloat  you {}/{n} · {opp} {}/{n}",
            v.afloat(0),
            v.afloat(1)
        );
        let mut key = legend(color, true);
        if AFLOAT_AT + afloat.chars().count() < INNER {
            key.pad(AFLOAT_AT).push(&afloat);
            lines.push(Line::Full(key));
        } else {
            lines.push(Line::Full(key));
            full(&mut lines, &afloat);
        }
    }

    let (status, hint) = bridge_status(v, opp);
    let moment = match &v.phase {
        ViewPhase::Placing { .. } => "placing".to_string(),
        ViewPhase::Battle { .. } => format!("shot {}", v.shot_number()),
        ViewPhase::Reveal { .. } => "reveal".into(),
        ViewPhase::Over { .. } => "over".into(),
    };
    let title_left = "╭─ 0sum · battleship ";
    let room = WIDTH - title_left.chars().count() - format!("─  vs  · {moment} ─╮").chars().count();
    let (mine, opp_short) = clip_pair(mine, opp, room);
    let title_right = format!(" {mine} vs {opp_short} · {moment} ─");
    let hint_fits = format!("╰─ {status} ▶ {hint} ─╯").chars().count() <= WIDTH;
    if !hint_fits {
        full(&mut lines, &format!("▶ {hint}"));
    }
    if let Some(e) = &v.error {
        let mut top = Vec::new();
        full(&mut top, &format!("error: {e}"));
        lines.splice(0..0, top);
    }

    let (width, inner, left_w, right_w) = (WIDTH, INNER, LEFT_W, RIGHT_W);

    let frame = |s: &str| {
        let mut t = Span::default();
        t.paint(FRAME, s, color);
        t.s
    };
    let mut out = Vec::new();
    let fill = width - title_left.chars().count() - title_right.chars().count() - 1;
    let mut top = Span::default();
    top.paint(FRAME, "╭─ ", color)
        .paint(BOLD, "0sum · battleship", color)
        .paint(
            FRAME,
            &format!(" {}{title_right}╮", "─".repeat(fill)),
            color,
        );
    out.push(top.s);
    for l in lines {
        match l {
            Line::Split(mut a, mut b) => {
                a.pad(left_w);
                b.pad(right_w);
                out.push(format!("{} {}{}{}", frame("│"), a.s, b.s, frame("│")));
            }
            Line::Full(mut a) => {
                a.pad(inner);
                out.push(format!("{} {}{}", frame("│"), a.s, frame("│")));
            }
            Line::Rule => out.push(frame(&format!("├{}┤", "─".repeat(width - 2)))),
        }
    }
    let mut bottom = Span::default();
    bottom
        .paint(FRAME, "╰─ ", color)
        .paint(BOLD, &status, color);
    if hint_fits {
        bottom.push(&format!(" ▶ {hint} "));
    } else {
        bottom.push(" ");
    }
    let fill = width - bottom.w - 1;
    bottom.paint(FRAME, &format!("{}╯", "─".repeat(fill)), color);
    out.push(bottom.s);
    out.join("\n")
}

fn legend(color: bool, battle: bool) -> Span {
    let mut t = Span::default();
    t.paint(FRAME, "·", color).push(" water  ");
    t.paint(SHIP, "■", color).push(" ship");
    if battle {
        t.push("  ").paint(HIT, "✕", color).push(" hit  ");
        t.paint(SUNK, "▓", color).push(" sunk  ");
        t.paint(MISS, "○", color).push(" miss");
    }
    t
}

/// Plain text as frame-wide lines, wrapped at spaces.
fn full(lines: &mut Vec<Line>, s: &str) {
    lines.extend(wrap(s, INNER - 1).iter().map(|l| Line::Full(text(l))));
}

/// `s` broken at spaces into lines of at most `w` columns; later lines are
/// indented two.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut fresh = true;
    for word in s.split(' ') {
        if !fresh && line.chars().count() + 1 + word.chars().count() > w {
            out.push(std::mem::replace(&mut line, "  ".into()));
            fresh = true;
        }
        if !fresh {
            line.push(' ');
        }
        line.push_str(word);
        fresh = false;
    }
    out.push(line);
    out
}

/// Both names in `room` columns, the longer one cut first, with `…`.
fn clip_pair<'a>(a: &'a str, b: &'a str, room: usize) -> (String, String) {
    let (na, nb) = (a.chars().count(), b.chars().count());
    if na + nb <= room {
        return (a.into(), b.into());
    }
    let half = room / 2;
    let (wa, wb) = if na <= half {
        (na, room - na)
    } else if nb <= half {
        (room - nb, nb)
    } else {
        (room - half, half)
    };
    let clip = |s: &str, n: usize, w: usize| {
        if n <= w {
            s.to_string()
        } else {
            format!("{}…", s.chars().take(w - 1).collect::<String>())
        }
    };
    (clip(a, na, wa), clip(b, nb, wb))
}

fn bridge_status(v: &View, opp: &str) -> (String, String) {
    let voice = Voice {
        me: "you",
        my: "your",
        them: opp.to_string(),
        their: format!("{opp}'s"),
    };
    match &v.phase {
        ViewPhase::Placing { locked: true, .. } => (
            "locked".into(),
            format!("waiting on {opp} to place their fleet"),
        ),
        ViewPhase::Placing { .. } => {
            let placed = v
                .own
                .as_ref()
                .map_or(0, |o| o.placed.iter().filter(|p| p.is_some()).count());
            (
                "placing".into(),
                format!("{placed} of {} placed", v.fleet.len()),
            )
        }
        ViewPhase::Battle { my_turn: true } => (
            "your turn".into(),
            match v.suggestion() {
                Some(i) => format!("zs play fire <square>  e.g. zs play fire {}", v.sq(i)),
                None => "zs play fire <square>".into(),
            },
        ),
        ViewPhase::Battle { .. } => match v.last_mine {
            Some(ShotView { cell, result: None }) => (
                "waiting".into(),
                format!("{opp}'s answer to your shot at {}", v.sq(cell)),
            ),
            _ => (format!("{opp}'s turn"), format!("waiting on {opp}'s shot")),
        },
        _ => {
            let words = ending(v, &voice).expect("Reveal and Over have an ending");
            let status = match &v.phase {
                ViewPhase::Reveal { .. } => "reveal",
                _ => "over",
            };
            (status.into(), words)
        }
    }
}
