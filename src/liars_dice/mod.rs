//! Liar's Dice on the engine (PHASE2 §4, §7.2, §10). Dice are hidden and fair:
//! commit → entropy → derive, then every live seat reveals on a call. `fold`
//! checks all of it over public data alone; secrets only produce events
//! (`duty`) and render your own dice.

pub mod dice;
pub mod ruleset;

use serde::{Deserialize, Serialize};

use crate::engine::log::{Hex, Hex32};
use crate::engine::{Ctx, Game, SessionInfo};
use ruleset::RuleSet;

pub struct LiarsDice;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Call {
    Liar,
    Spot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Commit { round: u32, c: Hex32 },
    Entropy { round: u32, n: Hex32 },
    Bid { qty: u8, face: u8 },
    Call { kind: Call },
    Reveal { round: u32, s: Hex32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Bid(u8, u8),
    Liar,
    Spot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Rolling,
    Bidding,
    Revealing,
    Over,
}

/// A resolved round, kept for rendering: the last bid, the call, every live
/// seat's revealed dice, the count, and who lost a die.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundResult {
    pub round: u32,
    pub bidder: usize,
    pub qty: u8,
    pub face: u8,
    pub caller: usize,
    pub call: Call,
    pub dice: Vec<Option<Vec<u8>>>,
    pub count: u32,
    pub loser: usize,
    pub left: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub rules: RuleSet,
    sid: String,
    seats: Vec<String>,
    /// Held per seat; 0 = eliminated.
    pub dice: Vec<u8>,
    /// 1-based.
    pub round: u32,
    pub phase: Phase,
    pub starter: usize,
    pub turn: usize,
    commits: Vec<Option<[u8; 32]>>,
    /// round + 1, gathered during Revealing.
    next_commits: Vec<Option<[u8; 32]>>,
    entropy: Vec<Option<[u8; 32]>>,
    /// (author, qty, face)
    pub bids: Vec<(usize, u8, u8)>,
    pub call: Option<(usize, Call)>,
    revealed: Vec<Option<Vec<u8>>>,
    pub last: Option<RoundResult>,
    pub winner: Option<usize>,
}

impl State {
    fn live(&self, i: usize) -> bool {
        self.dice[i] > 0
    }

    fn live_seats(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.dice.len()).filter(|&i| self.live(i))
    }

    fn next_live(&self, a: usize) -> usize {
        let n = self.dice.len();
        (1..=n)
            .map(|k| (a + k) % n)
            .find(|&i| self.live(i))
            .unwrap_or(a)
    }

    fn dice_in_play(&self) -> u32 {
        self.dice.iter().map(|&d| d as u32).sum()
    }

    /// Live seats whose `slot` is still empty.
    fn owing<T>(&self, slot: &[Option<T>]) -> Vec<usize> {
        self.live_seats().filter(|&i| slot[i].is_none()).collect()
    }

    fn all_live(&self, slot: &[Option<impl Sized>]) -> bool {
        self.live_seats().all(|i| slot[i].is_some())
    }

    fn cset(&self) -> [u8; 32] {
        dice::commitment_set(self.live_seats().filter_map(|i| self.commits[i].as_ref()))
    }

    /// Every other live seat's entropy, in seat order — `None` until all are in.
    fn others_entropy(&self, me: usize) -> Option<Vec<[u8; 32]>> {
        self.live_seats()
            .filter(|&k| k != me)
            .map(|k| self.entropy[k])
            .collect()
    }

    fn count(&self, face: u8) -> u32 {
        let wild = self.rules.wild_ones && face != 1;
        self.revealed
            .iter()
            .flatten()
            .flatten()
            .filter(|&&d| d == face || (wild && d == 1))
            .count() as u32
    }

    fn resolve(&mut self) {
        let (bidder, qty, face) = *self.bids.last().expect("a call needs a bid");
        let (caller, call) = self.call.expect("Revealing implies a call");
        let count = self.count(face);
        let loser = match call {
            Call::Liar if count >= qty as u32 => caller,
            Call::Liar => bidder,
            Call::Spot if count == qty as u32 => bidder,
            Call::Spot => caller,
        };
        self.dice[loser] -= 1;
        self.last = Some(RoundResult {
            round: self.round,
            bidder,
            qty,
            face,
            caller,
            call,
            dice: self.revealed.clone(),
            count,
            loser,
            left: self.dice[loser],
        });
        let live: Vec<usize> = self.live_seats().collect();
        if let [w] = live[..] {
            self.winner = Some(w);
            self.phase = Phase::Over;
            return;
        }
        let n = self.dice.len();
        self.round += 1;
        self.starter = if self.live(loser) {
            loser
        } else {
            self.next_live(loser)
        };
        self.turn = self.starter;
        self.commits = (0..n)
            .map(|i| self.next_commits[i].filter(|_| self.live(i)))
            .collect();
        self.next_commits = vec![None; n];
        self.entropy = vec![None; n];
        self.bids.clear();
        self.call = None;
        self.revealed = vec![None; n];
        self.phase = Phase::Rolling;
    }
}

fn fmt_dice(dice: &[u8]) -> String {
    let mut d = dice.to_vec();
    d.sort_unstable();
    d.iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn list(names: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    names
        .into_iter()
        .map(|n| n.as_ref().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

const MOVES_HINT: &str = "try: bid <qty> <face> | liar | spot";

fn result_lines(r: &RoundResult, rules: &RuleSet, names: &[String]) -> Vec<String> {
    let call = match r.call {
        Call::Liar => "liar",
        Call::Spot => "spot-on",
    };
    let mut out = vec![format!(
        "Round {} result: {} called {call} on {}'s {}×{}.",
        r.round, names[r.caller], names[r.bidder], r.qty, r.face
    )];
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);
    for (i, d) in r.dice.iter().enumerate() {
        if let Some(d) = d {
            out.push(format!("  {:<width$}  {}", names[i], fmt_dice(d)));
        }
    }
    let wild = if rules.wild_ones && r.face != 1 {
        " (1s wild)"
    } else {
        ""
    };
    let verdict = match r.call {
        Call::Liar if r.count >= r.qty as u32 => "the bid holds",
        Call::Liar => "the bid was a lie",
        Call::Spot if r.count == r.qty as u32 => "exactly right",
        Call::Spot => "not exact",
    };
    let fate = if r.left == 0 {
        format!("{} loses their last die and is out.", names[r.loser])
    } else {
        format!("{} loses a die ({} left).", names[r.loser], r.left)
    };
    out.push(format!(
        "  {}s showing: {}{wild} — {verdict}. {fate}",
        r.face, r.count
    ));
    out
}

impl Game for LiarsDice {
    const ID: &'static str = "liars-dice";
    const DEFAULT_RULESET: &'static str = "classic";
    type Rules = RuleSet;
    type State = State;
    type Move = Move;
    type Event = Event;

    fn ruleset(name: &str) -> Result<RuleSet, String> {
        RuleSet::by_name(name)
    }

    fn rules_name(rules: &RuleSet) -> &str {
        &rules.name
    }

    fn describe(rules: &RuleSet) -> String {
        let mut extras = Vec::new();
        if rules.wild_ones {
            extras.push("1s wild");
        }
        if rules.spot_on {
            extras.push("spot-on");
        }
        format!(
            "Liar's Dice, ruleset {} ({} dice each{}{})",
            rules.name,
            rules.dice_per_player,
            if extras.is_empty() { "" } else { ", " },
            extras.join(", ")
        )
    }

    fn initial(rules: RuleSet, info: &SessionInfo) -> State {
        let n = info.seats.len();
        State {
            sid: info.session_id.clone(),
            seats: info.seats.clone(),
            dice: vec![rules.dice_per_player; n],
            round: 1,
            phase: Phase::Rolling,
            starter: 0,
            turn: 0,
            commits: vec![None; n],
            next_commits: vec![None; n],
            entropy: vec![None; n],
            bids: Vec::new(),
            call: None,
            revealed: vec![None; n],
            last: None,
            winner: None,
            rules,
        }
    }

    /// Exactly one move: Liar's Dice has no legal chains.
    fn parse_moves(tokens: &[String]) -> Result<Vec<Move>, String> {
        let (verb, args) = tokens
            .split_first()
            .ok_or_else(|| format!("no move given ({MOVES_HINT})"))?;
        let mv = match verb.to_ascii_lowercase().as_str() {
            "bid" | "b" => {
                let [q, f] = args else {
                    return Err("bid takes a quantity and a face, e.g. `bid 3 5`".into());
                };
                let num = |t: &String| {
                    t.parse::<u8>()
                        .map_err(|_| format!("`{t}` is not a number"))
                };
                return Ok(vec![Move::Bid(num(q)?, num(f)?)]);
            }
            "liar" | "l" | "call" => Move::Liar,
            "spot" | "exact" => Move::Spot,
            other => return Err(format!("unknown move `{other}` ({MOVES_HINT})")),
        };
        if !args.is_empty() {
            return Err("Liar's Dice takes one move per invocation".into());
        }
        Ok(vec![mv])
    }

    fn fold(st: &mut State, a: usize, ev: &Event) -> Result<(), String> {
        if st.phase == Phase::Over {
            return Err("the game is over".into());
        }
        let seat = st.seats[a].clone();
        match ev {
            Event::Commit { round: k, c } => {
                if st.phase == Phase::Rolling && *k == st.round {
                    if !st.live(a) {
                        return Err(format!("{seat} is out and can't commit"));
                    }
                    if st.commits[a].is_some() {
                        return Err(format!("{seat} already committed for round {k}"));
                    }
                    st.commits[a] = Some(c.0);
                } else if st.phase == Phase::Revealing && *k == st.round + 1 {
                    if st.revealed[a].is_none() {
                        return Err(format!(
                            "{seat} committed to round {k} before revealing round {}",
                            st.round
                        ));
                    }
                    if st.next_commits[a].is_some() {
                        return Err(format!("{seat} already committed for round {k}"));
                    }
                    st.next_commits[a] = Some(c.0);
                } else {
                    return Err(format!(
                        "a commit for round {k} doesn't fit round {} ({:?})",
                        st.round, st.phase
                    ));
                }
            }
            Event::Entropy { round: k, n } => {
                if st.phase != Phase::Rolling || *k != st.round {
                    return Err(format!(
                        "entropy for round {k} doesn't fit round {} ({:?})",
                        st.round, st.phase
                    ));
                }
                if !st.live(a) {
                    return Err(format!("{seat} is out and can't publish entropy"));
                }
                if st.entropy[a].is_some() {
                    return Err(format!("{seat} already published entropy for round {k}"));
                }
                if !st.all_live(&st.commits) {
                    return Err(format!(
                        "entropy for round {k} before every live seat committed"
                    ));
                }
                st.entropy[a] = Some(n.0);
                if st.all_live(&st.entropy) {
                    st.phase = Phase::Bidding;
                    st.turn = st.starter;
                }
            }
            Event::Bid { qty, face } => {
                if st.phase != Phase::Bidding {
                    return Err("no bidding right now".into());
                }
                if a != st.turn {
                    return Err(format!("{seat} bid out of turn"));
                }
                let lo = st.rules.lowest_bid_face();
                if !(lo..=6).contains(face) {
                    return Err(if st.rules.wild_ones {
                        format!("can't bid {face}s: 1s are wild, bid a face 2-6")
                    } else {
                        format!("can't bid {face}s: faces are 1-6")
                    });
                }
                let total = st.dice_in_play();
                if *qty < 1 || *qty as u32 > total {
                    return Err(format!(
                        "a bid's quantity must be 1-{total} ({total} dice in play)"
                    ));
                }
                if let Some(&(_, q0, f0)) = st.bids.last() {
                    if !(*qty > q0 || (*qty == q0 && *face > f0)) {
                        return Err(format!(
                            "{qty}×{face} doesn't beat {q0}×{f0} (raise the quantity, or the face at the same quantity)"
                        ));
                    }
                }
                st.bids.push((a, *qty, *face));
                st.turn = st.next_live(a);
            }
            Event::Call { kind } => {
                if st.phase != Phase::Bidding {
                    return Err("nothing to call right now".into());
                }
                if st.bids.is_empty() {
                    return Err("nothing to call yet: the round has no bid".into());
                }
                if a != st.turn {
                    return Err(format!("{seat} called out of turn"));
                }
                if *kind == Call::Spot && !st.rules.spot_on {
                    return Err(format!(
                        "spot-on isn't part of the {} ruleset",
                        st.rules.name
                    ));
                }
                st.call = Some((a, *kind));
                st.phase = Phase::Revealing;
            }
            Event::Reveal { round: k, s } => {
                if st.phase != Phase::Revealing || *k != st.round {
                    return Err(format!(
                        "a reveal for round {k} doesn't fit round {} ({:?})",
                        st.round, st.phase
                    ));
                }
                if !st.live(a) {
                    return Err(format!("{seat} is out and has nothing to reveal"));
                }
                if st.revealed[a].is_some() {
                    return Err(format!("{seat} already revealed round {k}"));
                }
                if Some(dice::commitment(&st.sid, *k, &seat, &s.0)) != st.commits[a] {
                    return Err(format!(
                        "the revealed secret doesn't open {seat}'s round {k} commitment"
                    ));
                }
                if Some(dice::entropy(&st.sid, *k, &seat, &s.0, &st.cset())) != st.entropy[a] {
                    return Err(format!(
                        "entropy for round {k} doesn't match the revealed secret"
                    ));
                }
                let others = st.others_entropy(a).expect("Revealing implies all entropy");
                st.revealed[a] = Some(dice::roll(&st.sid, *k, &seat, &s.0, &others, st.dice[a]));
                if st.all_live(&st.revealed) {
                    st.resolve();
                }
            }
        }
        Ok(())
    }

    fn plan(_st: &State, _me: usize, mv: &Move, _ctx: &mut Ctx) -> Result<Vec<Event>, String> {
        Ok(vec![match mv {
            Move::Bid(qty, face) => Event::Bid {
                qty: *qty,
                face: *face,
            },
            Move::Liar => Event::Call { kind: Call::Liar },
            Move::Spot => Event::Call { kind: Call::Spot },
        }])
    }

    /// §7.2, in order: reveal, pipelined next-round commit, commit, entropy.
    fn duty(st: &State, me: usize, ctx: &Ctx) -> Option<Event> {
        if !st.live(me) {
            return None;
        }
        let seat = &st.seats[me];
        let commit = |round: u32| {
            let s = dice::secret(&ctx.secrets, round);
            Event::Commit {
                round,
                c: Hex(dice::commitment(&st.sid, round, seat, &s)),
            }
        };
        match st.phase {
            Phase::Revealing if st.revealed[me].is_none() => Some(Event::Reveal {
                round: st.round,
                s: Hex(dice::secret(&ctx.secrets, st.round)),
            }),
            Phase::Revealing if st.next_commits[me].is_none() => Some(commit(st.round + 1)),
            Phase::Rolling if st.commits[me].is_none() => Some(commit(st.round)),
            Phase::Rolling if st.entropy[me].is_none() && st.all_live(&st.commits) => {
                let s = dice::secret(&ctx.secrets, st.round);
                Some(Event::Entropy {
                    round: st.round,
                    n: Hex(dice::entropy(&st.sid, st.round, seat, &s, &st.cset())),
                })
            }
            _ => None,
        }
    }

    fn to_move(st: &State) -> Option<usize> {
        (st.phase == Phase::Bidding).then_some(st.turn)
    }

    fn winner(st: &State) -> Option<usize> {
        st.winner
    }

    fn waiting(st: &State, names: &[String]) -> String {
        let (who, what) = match st.phase {
            Phase::Rolling => {
                let mut who = st.owing(&st.commits);
                if who.is_empty() {
                    who = st.owing(&st.entropy);
                }
                (who, "roll")
            }
            Phase::Revealing => (st.owing(&st.revealed), "reveal"),
            Phase::Bidding => (vec![st.turn], "bid"),
            Phase::Over => return "nobody: the game is over".into(),
        };
        match who[..] {
            [one] => format!("{}'s {what}", names[one]),
            _ => format!("{what}s from {}", list(who.iter().map(|&i| &names[i]))),
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
        match ev {
            Event::Commit { round, .. } => {
                vec![format!("{name} committed to round {round} dice")]
            }
            Event::Entropy { round, .. } => {
                let mut out = vec![format!("{name} rolled for round {round}")];
                if after.phase == Phase::Bidding {
                    out.push(format!(
                        "All dice are rolled. {} bids first.",
                        names[after.starter]
                    ));
                }
                out
            }
            Event::Bid { qty, face } => vec![format!("{name} bids {qty}×{face}")],
            Event::Call { kind } => {
                let (_, q, f) = *before.bids.last().expect("fold accepted the call");
                let (bidder, _, _) = *before.bids.last().unwrap();
                let call = match kind {
                    Call::Liar => "LIAR",
                    Call::Spot => "SPOT ON",
                };
                vec![format!(
                    "{name} calls {call} on {}'s {q}×{f}",
                    names[bidder]
                )]
            }
            Event::Reveal { .. } => {
                let mut out = Vec::new();
                let resolved = after.round != before.round || after.winner.is_some();
                let shown = if resolved {
                    after.last.as_ref().and_then(|r| r.dice[author].clone())
                } else {
                    after.revealed[author].clone()
                };
                out.push(format!(
                    "{name} reveals: {}",
                    fmt_dice(&shown.unwrap_or_default())
                ));
                if resolved {
                    let r = after.last.as_ref().expect("resolve records the round");
                    out.extend(result_lines(r, &after.rules, names));
                    if let Some(w) = after.winner {
                        out.push(format!("{} wins the game!", names[w]));
                    }
                }
                out
            }
        }
    }

    fn recap(before: &State, ev: &Event, names: &[String]) -> Option<String> {
        let last_bid = || {
            before
                .bids
                .last()
                .map(|&(b, q, f)| format!("{}'s {q}×{f}", names[b]))
                .unwrap_or_default()
        };
        match ev {
            Event::Bid { qty, face } => Some(format!("bid {qty}×{face}")),
            Event::Call { kind: Call::Liar } => Some(format!("called liar on {}", last_bid())),
            Event::Call { kind: Call::Spot } => Some(format!("called spot-on on {}", last_bid())),
            _ => None,
        }
    }

    fn moment(before: &State) -> Option<String> {
        Some(format!("round {}", before.round))
    }

    fn render(st: &State, names: &[String], me: Option<usize>, ctx: Option<&Ctx>) -> String {
        let mut out = Vec::new();
        if let Some(r) = &st.last {
            if st.bids.is_empty() || st.phase == Phase::Over {
                out.extend(result_lines(r, &st.rules, names));
                out.push(String::new());
            }
        }
        out.push(format!(
            "--- {} · round {} ---",
            Self::describe(&st.rules),
            st.round
        ));
        let width = names.iter().map(|n| n.len()).max().unwrap_or(0);
        for (i, name) in names.iter().enumerate() {
            let marker = if st.phase == Phase::Bidding && i == st.turn {
                "*"
            } else {
                " "
            };
            let held = match st.dice[i] {
                0 => "out".to_string(),
                1 => "1 die".to_string(),
                d => format!("{d} dice"),
            };
            out.push(format!("  {marker} {name:<width$}  {held}"));
        }
        if !st.bids.is_empty() {
            let bids: Vec<String> = st
                .bids
                .iter()
                .map(|&(b, q, f)| format!("{} {q}×{f}", names[b]))
                .collect();
            out.push(format!("  bids: {}", bids.join(" · ")));
        }
        if let (Some(me), Some(ctx)) = (me, ctx) {
            if st.phase != Phase::Over {
                out.push(your_dice(st, me, ctx, names));
            }
        }
        out.push(String::new());
        out.push(match st.phase {
            Phase::Over => format!(
                ">>> {} WINS! <<<",
                names[st.winner.expect("Over has a winner")]
            ),
            Phase::Bidding if Some(st.turn) == me => {
                let mut opts = vec!["zs play bid <qty> <face>".to_string()];
                if let Some(&(_, q, f)) = st.bids.last() {
                    opts[0].push_str(&format!(" (beat {q}×{f})"));
                    opts.push("zs play liar".into());
                    if st.rules.spot_on {
                        opts.push("zs play spot".into());
                    }
                }
                format!(
                    "Your move, {} dice in play: {}",
                    st.dice_in_play(),
                    opts.join(" | ")
                )
            }
            Phase::Bidding => format!("{}'s turn to bid", names[st.turn]),
            _ => format!("waiting on {}", Self::waiting(st, names)),
        });
        out.join("\n")
    }
}

fn your_dice(st: &State, me: usize, ctx: &Ctx, names: &[String]) -> String {
    if !st.live(me) {
        return "  you're out".into();
    }
    match st.others_entropy(me) {
        Some(others) => {
            let s = dice::secret(&ctx.secrets, st.round);
            let d = dice::roll(&st.sid, st.round, &st.seats[me], &s, &others, st.dice[me]);
            format!("  your dice: {}", fmt_dice(&d))
        }
        None => {
            let missing = st
                .live_seats()
                .filter(|&k| k != me && st.entropy[k].is_none())
                .map(|k| names[k].as_str());
            format!("  your dice: waiting on {} to roll", list(missing))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::log::{Body, Entry, Session};
    use crate::engine::testkit::{player, seated, Player};
    use crate::engine::{apply_chain, replay, Broken, Chain, Table};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use serde_json::json;

    type Seated = (Session, Table<LiarsDice>, Vec<Player>);

    fn go(g: &mut Seated, seat: usize, moves: &[Move]) -> Chain<LiarsDice> {
        let (s, t, players) = g;
        let p = &mut players[seat];
        apply_chain(s, t, seat, moves, &mut p.ctx, &p.sk)
    }

    fn body(s: &Session, seq: usize) -> serde_json::Value {
        match &s.log[seq].body {
            Body::Game(v) => v.clone(),
            other => panic!("seq {seq} is {other:?}"),
        }
    }

    fn render_for(g: &Seated, seat: usize) -> String {
        let (_, t, players) = g;
        LiarsDice::render(&t.state, &t.names, Some(seat), Some(&players[seat].ctx))
    }

    /// PHASE2 §7.3, step by step, with the §13 passwords.
    fn walkthrough() -> Seated {
        let mut g = seated::<LiarsDice>(&RuleSet::classic(), &["Alice", "Bob"]);
        assert_eq!(go(&mut g, 1, &[]).steps.len(), 1); // 3 commit
        assert_eq!(go(&mut g, 0, &[]).steps.len(), 2); // 4 commit, 5 entropy
        assert_eq!(go(&mut g, 1, &[]).steps.len(), 1); // 6 entropy → Bidding
        assert!(render_for(&g, 1).contains("your dice: 1 1 4 4 5"));
        assert!(render_for(&g, 0).contains("your dice: 1 3 4 4 4"));
        for (seat, mv) in [
            (0, Move::Bid(2, 6)),
            (1, Move::Bid(3, 5)),
            (0, Move::Bid(3, 6)),
        ] {
            assert_eq!(go(&mut g, seat, &[mv]).failure, None);
        }
        let c = go(&mut g, 1, &[Move::Liar]); // 10 call, 11 reveal, 12 commit{2}
        assert_eq!((c.failure, c.steps.len()), (None, 3));
        let c = go(&mut g, 0, &[]); // 13 reveal → resolve, 14 commit{2}, 15 entropy{2}
        assert_eq!((c.failure, c.steps.len()), (None, 3));
        let c = go(&mut g, 1, &[]); // 16 entropy{2}
        assert_eq!((c.failure, c.steps.len()), (None, 1));
        g
    }

    #[test]
    fn walkthrough_matches_the_spec_round() {
        let g = walkthrough();
        let (s, t, _) = &g;
        let shape: Vec<(Option<&str>, String)> = s.log[3..]
            .iter()
            .map(|e| {
                let Body::Game(v) = &e.body else {
                    unreachable!()
                };
                let kind = v.as_object().unwrap().keys().next().unwrap().clone();
                (e.author.as_deref(), kind)
            })
            .collect();
        let want = [
            ("p2", "commit"),
            ("p1", "commit"),
            ("p1", "entropy"),
            ("p2", "entropy"),
            ("p1", "bid"),
            ("p2", "bid"),
            ("p1", "bid"),
            ("p2", "call"),
            ("p2", "reveal"),
            ("p2", "commit"),
            ("p1", "reveal"),
            ("p1", "commit"),
            ("p1", "entropy"),
            ("p2", "entropy"),
        ];
        let want: Vec<(Option<&str>, String)> = want
            .iter()
            .map(|(a, k)| (Some(*a), k.to_string()))
            .collect();
        assert_eq!(shape, want);

        // The §13 vectors land in the log verbatim.
        assert_eq!(
            body(s, 3),
            json!({"commit": {"round": 1, "c": "9dddb09c7c16cc8a2cba8b6db9ec56469d97580477b6da97f264d93f4b1c72dd"}})
        );
        assert_eq!(
            body(s, 5),
            json!({"entropy": {"round": 1, "n": "4b5815a8afe6b4db7eda09da30246c1cc116dc66476f6b11345420c3dd265d7c"}})
        );
        assert_eq!(
            body(s, 11),
            json!({"reveal": {"round": 1, "s": "24480e09e67297d9b0c2cf278727d3df6c636c2d122d01a7ed6f47421bd8ad9e"}})
        );

        // 3×6 with no sixes but three wild 1s is true: caller Bob loses.
        let r = t.state.last.as_ref().unwrap();
        assert_eq!((r.count, r.loser, r.left), (3, 1, 4));
        assert_eq!(
            r.dice,
            vec![Some(vec![4, 3, 4, 1, 4]), Some(vec![5, 4, 1, 4, 1])]
        );
        assert_eq!(t.state.dice, vec![5, 4]);
        assert_eq!((t.state.round, t.state.starter), (2, 1));
        assert_eq!(LiarsDice::to_move(&t.state), Some(1));
        let bob = render_for(&g, 1);
        assert!(bob.contains("Round 1 result: Bob called liar on Alice's 3×6."));
        assert!(bob.contains("6s showing: 3 (1s wild) — the bid holds. Bob loses a die (4 left)."));
        assert!(bob.contains("Your move, 9 dice in play: zs play bid <qty> <face>"));
        assert_eq!(
            t.recap_line(1),
            "your last move: called liar on Alice's 3×6 (round 1, seq 10)"
        );

        let replayed = replay::<LiarsDice>(s).unwrap();
        assert_eq!(replayed.state, t.state);
        assert_eq!(replayed.last_move, t.last_move);
    }

    #[test]
    fn verify_narrates_the_resolution() {
        let (s, _, _) = walkthrough();
        let mut out = Vec::new();
        crate::engine::replay_narrated::<LiarsDice>(&s, &mut out).unwrap();
        let text = out.join("\n");
        assert!(text.contains("  10  Bob calls LIAR on Alice's 3×6"));
        assert!(text.contains("  13  Alice reveals: 1 3 4 4 4"));
        assert!(text.contains("  13    Alice  1 3 4 4 4"));
        assert!(text.contains("  13    Bob    1 1 4 4 5"));
    }

    // ---- tamper suite, against the full round ----

    fn rehash(e: &mut Entry) {
        e.hash = e.recompute_hash();
    }

    fn resign(e: &mut Entry, seat: usize) {
        let author = e.author.clone().unwrap();
        *e = Entry::signed(
            e.seq,
            e.ts,
            &author,
            e.prev,
            e.body.clone(),
            &player(seat).sk,
        );
    }

    fn broken(s: &Session) -> Broken {
        replay::<LiarsDice>(s).err().expect("replay must fail")
    }

    #[test]
    fn tamper_bid_qty_is_a_hash_mismatch_then_a_bad_signature() {
        let (mut s, _, _) = walkthrough();
        s.log[9].body = Body::Game(json!({"bid": {"qty": 4, "face": 6}}));
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 9: hash doesn't match the entry's contents"
        );
        rehash(&mut s.log[9]);
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 9: signature doesn't verify under the author's key"
        );
    }

    #[test]
    fn tamper_author_fails_the_signature() {
        let (mut s, _, _) = walkthrough();
        s.log[10].author = Some("p1".into());
        rehash(&mut s.log[10]);
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 10: signature doesn't verify under the author's key"
        );
    }

    #[test]
    fn tamper_deleting_a_middle_entry_breaks_prev() {
        let (mut s, _, _) = walkthrough();
        s.log.remove(8);
        for (i, e) in s.log.iter_mut().enumerate().skip(8) {
            e.seq = i as u64;
            rehash(e);
        }
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 8: prev doesn't match the previous entry's hash"
        );
    }

    #[test]
    fn tamper_unknown_field_fails_to_parse() {
        let (mut s, _, _) = walkthrough();
        let mut v = serde_json::to_value(&s).unwrap();
        v["log"][11]["extra"] = json!(1);
        assert!(serde_json::from_value::<Session>(v).is_err());
        s.log[11].body = Body::Game(json!({"reveal": {"round": 1, "s": "00", "dice": [6]}}));
        resign(&mut s.log[11], 1);
        let b = broken(&s);
        assert_eq!(b.seq, 11);
        assert!(b.reason.starts_with("event doesn't parse"), "{b}");
    }

    #[test]
    fn tamper_genesis_ruleset_after_register_breaks_seq_1() {
        let (mut s, _, _) = walkthrough();
        let Body::Genesis(g) = &mut s.log[0].body else {
            unreachable!()
        };
        g.ruleset = serde_json::to_value(RuleSet::natural()).unwrap();
        rehash(&mut s.log[0]);
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 1: prev doesn't match the previous entry's hash"
        );
    }

    #[test]
    fn a_ruleset_must_equal_its_builtin() {
        let (mut s, _, _) = walkthrough();
        let Body::Genesis(g) = &mut s.log[0].body else {
            unreachable!()
        };
        g.ruleset["spot_on"] = json!(true);
        rehash(&mut s.log[0]);
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 0: ruleset `classic` differs from the built-in one of that name"
        );
    }

    /// Push a validly signed entry without folding it first.
    fn forge(s: &mut Session, seat: usize, ev: serde_json::Value) {
        let prev = s.log.last().unwrap().hash;
        let seq = s.log.len() as u64;
        let e = Entry::signed(
            seq,
            0,
            &format!("p{}", seat + 1),
            prev,
            Body::Game(ev),
            &player(seat).sk,
        );
        s.log.push(e);
    }

    #[test]
    fn entropy_before_the_last_commit_is_rejected() {
        let mut g = seated::<LiarsDice>(&RuleSet::classic(), &["Alice", "Bob"]);
        go(&mut g, 0, &[]); // 3 Alice commits; she can't publish entropy yet
        let (mut s, t, players) = g;
        assert_eq!(s.log.len(), 4);
        let sa = dice::secret(&players[0].ctx.secrets, 1);
        let n = dice::entropy("TEST01", 1, "p1", &sa, &[0; 32]);
        let mut st = t.state.clone();
        let ev = Event::Entropy {
            round: 1,
            n: Hex(n),
        };
        assert_eq!(
            LiarsDice::fold(&mut st, 0, &ev).unwrap_err(),
            "entropy for round 1 before every live seat committed"
        );
        forge(&mut s, 0, serde_json::to_value(&ev).unwrap());
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 4: entropy for round 1 before every live seat committed"
        );
    }

    #[test]
    fn a_wrong_secret_does_not_open_the_commitment() {
        let (mut s, _, _) = walkthrough();
        s.log.truncate(11); // through Bob's call
        let wrong = hex::encode(dice::secret(&player(0).ctx.secrets, 1));
        forge(&mut s, 1, json!({"reveal": {"round": 1, "s": wrong}}));
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 11: the revealed secret doesn't open p2's round 1 commitment"
        );
    }

    /// A3: entropy that wasn't derived from the committed secret is caught at reveal.
    #[test]
    fn forged_entropy_is_caught_at_reveal() {
        let mut g = seated::<LiarsDice>(&RuleSet::classic(), &["Alice", "Bob"]);
        go(&mut g, 1, &[]); // 3 Bob commits
        go(&mut g, 0, &[]); // 4, 5 Alice commits + entropy
        {
            let (s, t, players) = &mut g;
            let ev = Event::Entropy {
                round: 1,
                n: Hex([7; 32]),
            };
            crate::engine::append(s, t, 1, ev, &players[1].sk).unwrap(); // 6: chosen, not derived
        }
        assert_eq!(go(&mut g, 0, &[Move::Bid(2, 6)]).failure, None);
        let c = go(&mut g, 1, &[Move::Liar]);
        assert_eq!(
            c.failure.as_deref(),
            Some("entropy for round 1 doesn't match the revealed secret")
        );
        let (mut s, _, players) = g;
        let sb = hex::encode(dice::secret(&players[1].ctx.secrets, 1));
        forge(&mut s, 1, json!({"reveal": {"round": 1, "s": sb}}));
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 9: entropy for round 1 doesn't match the revealed secret"
        );
    }

    /// Rolling the file back can't re-roll anything: an honest binary
    /// republishes byte-identical commits and entropy.
    #[test]
    fn duties_from_a_truncated_prefix_are_deterministic() {
        let (full, _, _) = walkthrough();
        for cut in [4, 6, 14] {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let mut s = full.clone();
                s.log.truncate(cut);
                let mut t = replay::<LiarsDice>(&s).unwrap();
                let mut p = player(0);
                let c = apply_chain(&mut s, &mut t, 0, &[], &mut p.ctx, &p.sk);
                assert_eq!(c.failure, None);
                let mut p = player(1);
                apply_chain(&mut s, &mut t, 1, &[], &mut p.ctx, &p.sk);
                bodies.push(
                    s.log[cut..]
                        .iter()
                        .map(|e| serde_json::to_string(&e.body).unwrap())
                        .collect::<Vec<_>>(),
                );
            }
            assert!(!bodies[0].is_empty());
            assert_eq!(bodies[0], bodies[1]);
            let original: Vec<String> = full.log[cut..cut + bodies[0].len()]
                .iter()
                .map(|e| serde_json::to_string(&e.body).unwrap())
                .collect();
            assert_eq!(bodies[0], original, "cut at {cut}");
        }
    }

    // ---- cheating attempts ----

    fn sb(round: u32) -> [u8; 32] {
        dice::secret(&player(1).ctx.secrets, round)
    }

    fn sa(round: u32) -> [u8; 32] {
        dice::secret(&player(0).ctx.secrets, round)
    }

    fn honest(seat: usize, s: &mut Session, moves: &[Move]) {
        let mut t = replay::<LiarsDice>(s).unwrap();
        let mut p = player(seat);
        assert_eq!(
            apply_chain(s, &mut t, seat, moves, &mut p.ctx, &p.sk).failure,
            None
        );
    }

    fn game_hex(s: &Session, seq: usize, kind: &str, field: &str) -> [u8; 32] {
        let v = &body(s, seq)[kind][field];
        let mut out = [0; 32];
        hex::decode_to_slice(v.as_str().unwrap(), &mut out).unwrap();
        out
    }

    /// Round 2 of the walkthrough, with Bob on a modified binary: he commits
    /// after Alice, so her C and n are on the log for him to copy. Bob signs
    /// whatever `c`, `n` (a function of the CSET) and `s` he likes; Alice
    /// stays honest. Returns the replay of the finished round.
    fn bob_cheats_round_2(
        c: impl Fn(&Session) -> [u8; 32],
        n: impl Fn(&Session, [u8; 32]) -> [u8; 32],
        s: [u8; 32],
    ) -> Result<(), Broken> {
        let (mut log, _, _) = walkthrough();
        log.log.truncate(12); // through Bob's reveal{1}; he skips the pipelined commit
        honest(0, &mut log, &[]); // 12 reveal → round 2, 13 commit{2}
        let c = c(&log);
        forge(
            &mut log,
            1,
            json!({"commit": {"round": 2, "c": hex::encode(c)}}),
        ); // 14
        honest(0, &mut log, &[]); // 15 entropy{2}
        let cset = dice::commitment_set([&game_hex(&log, 13, "commit", "c"), &c]);
        let n = n(&log, cset);
        forge(
            &mut log,
            1,
            json!({"entropy": {"round": 2, "n": hex::encode(n)}}),
        ); // 16
        forge(&mut log, 1, json!({"bid": {"qty": 1, "face": 2}})); // 17
        honest(0, &mut log, &[Move::Liar]); // 18 call, 19 reveal, 20 commit{3}
        forge(
            &mut log,
            1,
            json!({"reveal": {"round": 2, "s": hex::encode(s)}}),
        ); // 21
        replay::<LiarsDice>(&log).map(|_| ())
    }

    fn honest_n(_: &Session, cset: [u8; 32]) -> [u8; 32] {
        dice::entropy("TEST01", 2, "p2", &sb(2), &cset)
    }

    #[test]
    fn commitments_bind_round_seat_and_session() {
        let own = |_: &Session| dice::commitment("TEST01", 2, "p2", &sb(2));
        bob_cheats_round_2(own, honest_n, sb(2)).expect("the honest control resolves");

        let doesnt_open =
            "BROKEN at seq 21: the revealed secret doesn't open p2's round 2 commitment";
        // Round 1's commitment replayed in round 2: neither secret opens it.
        let replayed = |s: &Session| game_hex(s, 3, "commit", "c");
        let n1 = |_: &Session, cset| dice::entropy("TEST01", 2, "p2", &sb(1), &cset);
        let b = bob_cheats_round_2(replayed, n1, sb(1)).unwrap_err();
        assert_eq!(b.to_string(), doesnt_open);
        let b = bob_cheats_round_2(replayed, honest_n, sb(2)).unwrap_err();
        assert_eq!(b.to_string(), doesnt_open);
        // Alice's commitment copied off the log, even opened with her real secret.
        let copied = |s: &Session| game_hex(s, 13, "commit", "c");
        let alice_c2 = game_hex(&walkthrough().0, 14, "commit", "c");
        assert_eq!(alice_c2, dice::commitment("TEST01", 2, "p1", &sa(2)));
        let na = |_: &Session, cset| dice::entropy("TEST01", 2, "p2", &sa(2), &cset);
        let b = bob_cheats_round_2(copied, na, sa(2)).unwrap_err();
        assert_eq!(b.to_string(), doesnt_open);
        // A commitment made for another session.
        let elsewhere = |_: &Session| dice::commitment("TEST02", 2, "p2", &sb(2));
        let b = bob_cheats_round_2(elsewhere, honest_n, sb(2)).unwrap_err();
        assert_eq!(b.to_string(), doesnt_open);
    }

    /// A3 beyond made-up bytes: entropy copied from the other seat, or
    /// replayed from an earlier round, doesn't match the revealed secret.
    #[test]
    fn copied_or_replayed_entropy_is_caught_at_reveal() {
        let own = |_: &Session| dice::commitment("TEST01", 2, "p2", &sb(2));
        let wrong = "BROKEN at seq 21: entropy for round 2 doesn't match the revealed secret";
        let alices = |s: &Session, _| game_hex(s, 15, "entropy", "n");
        let b = bob_cheats_round_2(own, alices, sb(2)).unwrap_err();
        assert_eq!(b.to_string(), wrong);
        let round_1 = |s: &Session, _| game_hex(s, 6, "entropy", "n");
        let b = bob_cheats_round_2(own, round_1, sb(2)).unwrap_err();
        assert_eq!(b.to_string(), wrong);
    }

    /// Reveals carry no dice, so swapping dice means changing an input to
    /// them: a commitment, an entropy, or whose seat an entry is. Every such
    /// edit breaks the log, even when the author re-signs their own entry.
    #[test]
    fn editing_commit_entropy_or_seat_to_swap_dice_breaks_the_log() {
        let (full, _, _) = walkthrough();
        let sig_fails =
            |seq| format!("BROKEN at seq {seq}: signature doesn't verify under the author's key");

        let mut s = full.clone();
        s.log[3].body = Body::Game(json!({"commit": {"round": 1, "c": hex::encode([1; 32])}}));
        rehash(&mut s.log[3]);
        assert_eq!(broken(&s).to_string(), sig_fails(3));

        let mut s = full.clone();
        s.log[6].body = Body::Game(json!({"entropy": {"round": 1, "n": hex::encode([1; 32])}}));
        rehash(&mut s.log[6]);
        assert_eq!(broken(&s).to_string(), sig_fails(6));

        // Bob rewrites his own entropy and re-signs it: Alice's later entries
        // still point at the original.
        let mut s = full.clone();
        s.log[6].body = Body::Game(json!({"entropy": {"round": 1, "n": hex::encode([1; 32])}}));
        resign(&mut s.log[6], 1);
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 7: prev doesn't match the previous entry's hash"
        );

        // Swap the two entropies' authors.
        let mut s = full.clone();
        s.log[5].author = Some("p2".into());
        s.log[6].author = Some("p1".into());
        rehash(&mut s.log[5]);
        assert_eq!(broken(&s).to_string(), sig_fails(5));

        // Swap Alice and Bob's seats in the roster.
        let mut s = full.clone();
        let Body::Genesis(g) = &mut s.log[0].body else {
            unreachable!()
        };
        g.roster.swap(0, 1);
        (g.roster[0].seat, g.roster[1].seat) = ("p1".into(), "p2".into());
        rehash(&mut s.log[0]);
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 1: prev doesn't match the previous entry's hash"
        );

        // Move Alice's register onto Bob's seat.
        let mut s = full.clone();
        s.log[1].body = Body::Register(crate::engine::log::Register {
            seat: "p2".into(),
            pubkey: Hex(player(0).sk.verifying_key().to_bytes()),
        });
        s.log[1].author = Some("p2".into());
        rehash(&mut s.log[1]);
        assert_eq!(broken(&s).to_string(), sig_fails(1));
    }

    /// A seat's own valid signature doesn't make its move legal on another
    /// seat's turn.
    #[test]
    fn a_bid_or_call_signed_by_the_wrong_seat_is_out_of_turn() {
        let (full, _, _) = walkthrough();
        let mut s = full.clone();
        s.log.truncate(7); // Bidding, Alice to move
        forge(&mut s, 1, json!({"bid": {"qty": 2, "face": 6}}));
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 7: p2 bid out of turn"
        );

        let mut s = full.clone();
        s.log.truncate(8); // Alice bid 2×6, Bob to move
        forge(&mut s, 0, json!({"bid": {"qty": 3, "face": 6}}));
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 8: p1 bid out of turn"
        );

        let mut s = full.clone();
        s.log.truncate(8);
        forge(&mut s, 0, json!({"call": {"kind": "liar"}}));
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 8: p1 called out of turn"
        );
    }

    #[test]
    fn a_signature_transplanted_to_another_entry_fails() {
        let (full, _, _) = walkthrough();
        // Bob's bid signature onto Bob's call.
        let mut s = full.clone();
        s.log[10].sig = s.log[8].sig;
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 10: signature doesn't verify under the author's key"
        );
        // Bob's whole signed bid replayed at the end of the log.
        let mut s = full.clone();
        let mut e = s.log[8].clone();
        s.log.push(e.clone());
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 17: seq is 8, expected 17"
        );
        e.seq = 17;
        e.prev = s.log[16].hash;
        rehash(&mut e);
        s.log[17] = e;
        assert_eq!(
            broken(&s).to_string(),
            "BROKEN at seq 17: signature doesn't verify under the author's key"
        );
    }

    // ---- rules ----

    fn bidding(rules: RuleSet, seats: usize) -> State {
        let info = SessionInfo {
            session_id: "TEST01".into(),
            seats: (1..=seats).map(|i| format!("p{i}")).collect(),
        };
        let mut st = LiarsDice::initial(rules, &info);
        st.phase = Phase::Bidding;
        st
    }

    fn bid(st: &mut State, q: u8, f: u8) -> Result<(), String> {
        let a = st.turn;
        LiarsDice::fold(st, a, &Event::Bid { qty: q, face: f })
    }

    #[test]
    fn bid_ordering_face_range_and_quantity_cap() {
        let mut st = bidding(RuleSet::classic(), 2);
        assert_eq!(
            bid(&mut st, 1, 1).unwrap_err(),
            "can't bid 1s: 1s are wild, bid a face 2-6"
        );
        assert!(bid(&mut st, 1, 7).is_err());
        assert_eq!(
            bid(&mut st, 0, 3).unwrap_err(),
            "a bid's quantity must be 1-10 (10 dice in play)"
        );
        assert!(bid(&mut st, 11, 2).is_err());
        // (q, f) beats (q0, f0) iff q > q0, or q == q0 and f > f0.
        let table: &[(u8, u8, bool)] = &[
            (2, 6, true),
            (2, 5, false),
            (2, 6, false),
            (1, 6, false),
            (3, 2, true),
            (3, 3, true),
            (3, 3, false),
            (4, 2, true),
            (10, 6, true),
        ];
        for &(q, f, ok) in table {
            assert_eq!(bid(&mut st, q, f).is_ok(), ok, "{q}×{f}");
        }
        assert_eq!(
            bid(&mut st, 10, 6).unwrap_err(),
            "10×6 doesn't beat 10×6 (raise the quantity, or the face at the same quantity)"
        );
        assert!(
            bid(&mut st, 11, 2).is_err(),
            "after (total, 6) only a call is left"
        );
        assert_eq!(st.turn, 1);

        let mut nat = bidding(RuleSet::natural(), 2);
        bid(&mut nat, 1, 1).unwrap();
        bid(&mut nat, 1, 2).unwrap();
    }

    #[test]
    fn calls_need_a_bid_the_turn_and_the_ruleset() {
        let mut st = bidding(RuleSet::classic(), 2);
        let liar = Event::Call { kind: Call::Liar };
        let spot = Event::Call { kind: Call::Spot };
        assert_eq!(
            LiarsDice::fold(&mut st, 0, &liar).unwrap_err(),
            "nothing to call yet: the round has no bid"
        );
        bid(&mut st, 2, 3).unwrap();
        assert!(
            LiarsDice::fold(&mut st.clone(), 0, &liar).is_err(),
            "not your turn"
        );
        assert_eq!(
            LiarsDice::fold(&mut st.clone(), 1, &spot).unwrap_err(),
            "spot-on isn't part of the classic ruleset"
        );
        LiarsDice::fold(&mut st, 1, &liar).unwrap();
        assert_eq!(st.phase, Phase::Revealing);

        let mut nat = bidding(RuleSet::natural(), 2);
        bid(&mut nat, 2, 3).unwrap();
        LiarsDice::fold(&mut nat, 1, &spot).unwrap();
    }

    #[test]
    fn wild_ones_count_toward_every_other_face() {
        let mut st = bidding(RuleSet::classic(), 2);
        st.revealed = vec![Some(vec![1, 6, 2, 6, 3]), Some(vec![1, 1, 4, 5, 6])];
        assert_eq!(st.count(6), 3 + 3);
        assert_eq!(st.count(2), 1 + 3);
        let mut nat = bidding(RuleSet::natural(), 2);
        nat.revealed = st.revealed.clone();
        assert_eq!(nat.count(6), 3);
        assert_eq!(nat.count(1), 3);
    }

    fn resolve_with(st: &mut State, bidder: usize, q: u8, f: u8, caller: usize, call: Call) {
        st.bids = vec![(bidder, q, f)];
        st.call = Some((caller, call));
        st.phase = Phase::Revealing;
        st.revealed = st
            .dice
            .iter()
            .map(|&d| (d > 0).then(|| vec![2; d as usize]))
            .collect();
        st.next_commits = vec![Some([9; 32]); st.dice.len()];
        st.resolve();
    }

    #[test]
    fn resolution_elimination_and_starter_skip() {
        // liar on a true bid: caller loses; on a false one: bidder loses.
        let mut st = bidding(RuleSet::classic(), 3);
        resolve_with(&mut st, 0, 15, 2, 1, Call::Liar);
        assert_eq!(st.dice, vec![5, 4, 5]);
        assert_eq!(st.starter, 1);
        resolve_with(&mut st, 0, 15, 2, 1, Call::Liar); // 14 twos now
        assert_eq!(st.dice, vec![4, 4, 5]);
        assert_eq!(st.starter, 0);

        // spot: exact → bidder loses; otherwise caller loses.
        let mut nat = bidding(RuleSet::natural(), 2);
        resolve_with(&mut nat, 0, 10, 2, 1, Call::Spot);
        assert_eq!(nat.dice, vec![4, 5]);
        resolve_with(&mut nat, 0, 8, 2, 1, Call::Spot);
        assert_eq!(nat.dice, vec![4, 4]);

        // The loser's last die: they're out, the next live seat starts,
        // and their pipelined commit is dropped.
        let mut st = bidding(RuleSet::classic(), 3);
        st.dice = vec![3, 1, 2];
        resolve_with(&mut st, 0, 6, 2, 1, Call::Liar);
        assert_eq!(st.dice, vec![3, 0, 2]);
        assert_eq!((st.starter, st.round, st.phase), (2, 2, Phase::Rolling));
        assert_eq!(st.commits, vec![Some([9; 32]), None, Some([9; 32])]);
        // Seat order skips the eliminated seat, wrapping around.
        assert_eq!(st.next_live(0), 2);
        assert_eq!(st.next_live(2), 0);

        // Last seat standing wins.
        st.dice = vec![1, 0, 2];
        resolve_with(&mut st, 2, 3, 2, 0, Call::Liar);
        assert_eq!(st.dice, vec![0, 0, 2]);
        assert_eq!((st.winner, st.phase), (Some(2), Phase::Over));
    }

    #[test]
    fn apply_chain_runs_duties_and_waits_on_rolls() {
        let mut g = seated::<LiarsDice>(&RuleSet::classic(), &["Alice", "Bob"]);
        let c = go(&mut g, 0, &[]);
        assert_eq!((c.failure, c.steps.len()), (None, 1));
        let c = go(&mut g, 0, &[Move::Bid(2, 3)]);
        assert_eq!(c.failure.as_deref(), Some("waiting on Bob's roll"));
        assert!(c.steps.is_empty());
        assert!(render_for(&g, 0).contains("your dice: waiting on Bob to roll"));
    }

    #[test]
    fn moves_parse_one_at_a_time() {
        let p = |line: &str| {
            let t: Vec<String> = line.split_whitespace().map(String::from).collect();
            LiarsDice::parse_moves(&t)
        };
        assert_eq!(p("bid 3 5").unwrap(), vec![Move::Bid(3, 5)]);
        assert_eq!(p("B 3 5").unwrap(), vec![Move::Bid(3, 5)]);
        assert_eq!(p("liar").unwrap(), vec![Move::Liar]);
        assert_eq!(p("l").unwrap(), vec![Move::Liar]);
        assert_eq!(p("call").unwrap(), vec![Move::Liar]);
        assert_eq!(p("exact").unwrap(), vec![Move::Spot]);
        assert!(p("bid 3").is_err());
        assert!(p("bid x 5").unwrap_err().contains("not a number"));
        assert!(p("liar liar").is_err());
        assert!(p("roll").unwrap_err().contains("unknown move"));
        assert!(p("").is_err());
    }

    /// Everyone runs their duties until nobody owes anything.
    fn settle(g: &mut Seated) {
        loop {
            let mut any = false;
            for seat in 0..g.1.names.len() {
                let c = go(g, seat, &[]);
                assert_eq!(c.failure, None);
                any |= !c.steps.is_empty();
            }
            if !any {
                return;
            }
        }
    }

    #[test]
    fn a_seeded_three_player_game_has_exactly_one_winner() {
        let mut g = seated::<LiarsDice>(&RuleSet::classic(), &["Alice", "Bob", "Carol"]);
        let mut rng = StdRng::seed_from_u64(7);
        let mut moves = 0;
        loop {
            settle(&mut g);
            let st = &g.1.state;
            if st.winner.is_some() {
                break;
            }
            moves += 1;
            assert!(moves < 10_000, "game failed to terminate");
            let me = LiarsDice::to_move(st).expect("after duties someone bids");
            let total = st.dice_in_play();
            let lo = st.rules.lowest_bid_face();
            let mv = match st.bids.last() {
                None => Move::Bid(1 + rng.gen_range(0..2), lo + rng.gen_range(0..3)),
                Some(&(_, q, 6)) if q as u32 == total => Move::Liar,
                Some(_) if rng.gen_bool(0.35) => Move::Liar,
                Some(&(_, q, 6)) => Move::Bid(q + 1, lo),
                Some(&(_, q, f)) => Move::Bid(q, f + 1),
            };
            assert_eq!(go(&mut g, me, &[mv]).failure, None);
        }
        let st = &g.1.state;
        let w = st.winner.unwrap();
        assert_eq!(st.dice.iter().filter(|&&d| d > 0).count(), 1);
        assert!(st.dice[w] > 0);
        assert_eq!(st.round as usize, 15 - st.dice[w] as usize);
        assert!(render_for(&g, 0).ends_with(&format!(">>> {} WINS! <<<", g.1.names[w])));
        let replayed = replay::<LiarsDice>(&g.0).unwrap();
        assert_eq!(replayed.state, g.1.state);
        assert_eq!(replayed.head, g.1.head);
    }
}
