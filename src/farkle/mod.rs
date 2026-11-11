//! Farkle on the engine (PHASE2 §9.6): signed and chained, gameplay unchanged.
//! Mechanics never hardcode scoring; they defer every value question to the
//! [`RuleSet`] via [`scoring`].

pub mod ruleset;
pub mod scoring;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::engine::{Ctx, Game, SessionInfo};
use ruleset::RuleSet;
use scoring::{has_any_score, score_selection};

pub struct Farkle;

/// A parsed player action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Roll,
    Keep(Vec<u8>),
    Bank,
}

/// Points are never stored: `fold` computes them, and a scoreless `rolled`
/// *is* the bust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Rolled { dice: Vec<u8> },
    Kept { dice: Vec<u8> },
    Banked {},
}

/// What action the current player may take next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    AwaitRoll,
    AwaitKeep,
    AwaitRollOrBank,
    GameOver,
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub rules: RuleSet,
    pub totals: Vec<u32>,
    pub turn: usize,
    pub winner: Option<usize>,
    pub turn_total: u32,
    pub dice_available: u8,
    pub table: Vec<u8>,
    pub phase: Phase,
}

impl State {
    fn end_turn(&mut self) {
        self.turn = (self.turn + 1) % self.totals.len();
        self.turn_total = 0;
        self.dice_available = self.rules.dice_count;
        self.table.clear();
        self.phase = Phase::AwaitRoll;
    }
}

enum Verb {
    Roll,
    Keep,
    Bank,
}

/// A token is either a verb or a die face, never both — that boundary is what
/// lets `keep` stop eating arguments when the next move starts.
fn as_verb(token: &str) -> Option<Verb> {
    match token.to_ascii_lowercase().as_str() {
        "roll" | "r" => Some(Verb::Roll),
        "keep" | "k" => Some(Verb::Keep),
        "bank" | "b" => Some(Verb::Bank),
        _ => None,
    }
}

impl Game for Farkle {
    const ID: &'static str = "farkle";
    const DEFAULT_RULESET: &'static str = "kcd";
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
        format!(
            "Farkle, ruleset {} (target {})",
            rules.name, rules.target_score
        )
    }

    fn initial(rules: RuleSet, info: &SessionInfo) -> State {
        State {
            totals: vec![0; info.seats.len()],
            turn: 0,
            winner: None,
            turn_total: 0,
            dice_available: rules.dice_count,
            table: Vec::new(),
            phase: Phase::AwaitRoll,
            rules,
        }
    }

    fn parse_moves(tokens: &[String]) -> Result<Vec<Move>, String> {
        let mut moves = Vec::new();
        let mut rest = tokens;
        while let Some((token, tail)) = rest.split_first() {
            let verb = as_verb(token).ok_or_else(|| {
                format!("unknown move `{token}` (try: roll | keep <dice...> | bank)")
            })?;
            rest = tail;
            match verb {
                Verb::Roll => moves.push(Move::Roll),
                Verb::Bank => moves.push(Move::Bank),
                Verb::Keep => {
                    let end = rest
                        .iter()
                        .position(|t| as_verb(t).is_some())
                        .unwrap_or(rest.len());
                    let (faces, tail) = rest.split_at(end);
                    rest = tail;
                    if faces.is_empty() {
                        return Err("keep needs dice, e.g. `keep 1 5`".into());
                    }
                    let dice = faces
                        .iter()
                        .map(|t| {
                            t.parse::<u8>()
                                .ok()
                                .filter(|d| (1..=6).contains(d))
                                .ok_or_else(|| format!("`{t}` is not a die face (1-6)"))
                        })
                        .collect::<Result<Vec<u8>, String>>()?;
                    moves.push(Move::Keep(dice));
                }
            }
        }
        if moves.is_empty() {
            return Err("no move given (try: roll | keep <dice...> | bank)".into());
        }
        Ok(moves)
    }

    fn fold(st: &mut State, author: usize, ev: &Event) -> Result<(), String> {
        if st.winner.is_some() {
            return Err("the game is over".into());
        }
        if author != st.turn {
            return Err(format!("seat p{} moved out of turn", author + 1));
        }
        match ev {
            Event::Rolled { dice } => {
                if st.phase == Phase::AwaitKeep {
                    return Err("you have dice on the table — `keep` scoring dice first".into());
                }
                if dice.len() != st.dice_available as usize {
                    return Err(format!(
                        "rolled {} dice with {} in hand",
                        dice.len(),
                        st.dice_available
                    ));
                }
                if !dice.iter().all(|d| (1..=6).contains(d)) {
                    return Err("a rolled die isn't a face 1-6".into());
                }
                if has_any_score(dice, &st.rules) {
                    st.table = dice.clone();
                    st.phase = Phase::AwaitKeep;
                } else {
                    st.end_turn();
                }
            }
            Event::Kept { dice } => {
                if st.phase != Phase::AwaitKeep {
                    return Err("nothing to keep — `roll` first".into());
                }
                if !is_submultiset(dice, &st.table) {
                    return Err(format!(
                        "those dice aren't all on the table (table: {})",
                        fmt_dice(&st.table)
                    ));
                }
                match score_selection(dice, &st.rules) {
                    Some(points) if points > 0 => {
                        st.turn_total += points;
                        st.dice_available = st.dice_available.saturating_sub(dice.len() as u8);
                        st.table.clear();
                        if st.dice_available == 0 {
                            st.dice_available = st.rules.dice_count; // hot dice
                        }
                        st.phase = Phase::AwaitRollOrBank;
                    }
                    _ => {
                        return Err(
                            "that isn't a valid scoring selection (every kept die must score)"
                                .into(),
                        )
                    }
                }
            }
            Event::Banked {} => {
                if st.phase != Phase::AwaitRollOrBank {
                    return Err("you can only bank after keeping scoring dice".into());
                }
                let cur = st.turn;
                if st.totals[cur] == 0 && st.turn_total < st.rules.on_board_minimum {
                    return Err(format!(
                        "you need at least {} in a turn to get on the board (have {})",
                        st.rules.on_board_minimum, st.turn_total
                    ));
                }
                st.totals[cur] += st.turn_total;
                if st.totals[cur] >= st.rules.target_score {
                    st.winner = Some(cur);
                    st.phase = Phase::GameOver;
                } else {
                    st.end_turn();
                }
            }
        }
        Ok(())
    }

    fn plan(st: &State, _me: usize, mv: &Move, ctx: &mut Ctx) -> Result<Vec<Event>, String> {
        Ok(vec![match mv {
            Move::Roll => Event::Rolled {
                dice: (0..st.dice_available)
                    .map(|_| ctx.rng.gen_range(1..=6))
                    .collect(),
            },
            Move::Keep(dice) => Event::Kept { dice: dice.clone() },
            Move::Bank => Event::Banked {},
        }])
    }

    fn to_move(st: &State) -> Option<usize> {
        st.winner.is_none().then_some(st.turn)
    }

    fn winner(st: &State) -> Option<usize> {
        st.winner
    }

    /// `before` names the actor (banking and busting both hand the turn on).
    fn narrate(
        before: &State,
        author: usize,
        ev: &Event,
        after: &State,
        names: &[String],
    ) -> Vec<String> {
        let name = &names[author];
        match ev {
            Event::Rolled { dice } => {
                let mut lines = vec![format!("{name} rolled: {}", fmt_dice(dice))];
                if after.phase != Phase::AwaitKeep {
                    lines.push(format!("BUST! No scoring dice. {name} loses this turn."));
                }
                lines
            }
            Event::Kept { .. } => vec![format!(
                "Kept for {} — turn total now {}.",
                after.turn_total - before.turn_total,
                after.turn_total
            )],
            Event::Banked {} => vec![format!(
                "{name} banked {} — total {}.",
                before.turn_total, after.totals[author]
            )],
        }
    }

    fn recap(before: &State, ev: &Event, _names: &[String]) -> Option<String> {
        Some(match ev {
            Event::Rolled { dice } => format!("rolled {}", fmt_dice(dice)),
            Event::Kept { dice } => format!("kept {}", fmt_dice(dice)),
            Event::Banked {} => format!("banked {}", before.turn_total),
        })
    }

    fn game_over(st: &State, names: &[String]) -> Option<String> {
        st.winner.map(|w| format!("Game over — {} won.", names[w]))
    }

    fn render(st: &State, names: &[String], _me: Option<usize>, _ctx: Option<&Ctx>) -> String {
        let mut out = vec![format!("--- scores (target {}) ---", st.rules.target_score)];
        let current = st.winner.is_none();
        for (i, name) in names.iter().enumerate() {
            let marker = if current && i == st.turn { "*" } else { " " };
            out.push(format!("  {marker} {name:<12} {}", st.totals[i]));
        }
        if current {
            if !st.table.is_empty() {
                out.push(format!("  table: {}", fmt_dice(&st.table)));
            }
            if st.turn_total > 0 {
                out.push(format!("  this turn (unbanked): {}", st.turn_total));
            }
        }
        out.push(String::new());
        out.push(match st.winner {
            Some(w) => format!(">>> {} WINS! <<<", names[w]),
            None => next_hint(st, &names[st.turn]),
        });
        out.join("\n")
    }
}

fn next_hint(st: &State, name: &str) -> String {
    match st.phase {
        Phase::AwaitRoll => format!("{name}'s turn — roll: zs play roll"),
        Phase::AwaitKeep => format!(
            "{name}: keep scoring dice from {}, e.g. zs play keep <dice...>",
            fmt_dice(&st.table)
        ),
        Phase::AwaitRollOrBank => format!(
            "{name}: roll again ({} dice) or bank {} — zs play roll | bank",
            st.dice_available, st.turn_total
        ),
        Phase::GameOver => "game over".into(),
    }
}

fn fmt_dice(dice: &[u8]) -> String {
    dice.iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Is every element of `sub` present in `sup` with at least the same multiplicity?
fn is_submultiset(sub: &[u8], sup: &[u8]) -> bool {
    let mut counts = [0i32; 7];
    for &d in sup {
        if (1..=6).contains(&d) {
            counts[d as usize] += 1;
        }
    }
    for &d in sub {
        if !(1..=6).contains(&d) {
            return false;
        }
        counts[d as usize] -= 1;
        if counts[d as usize] < 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::log::Session;
    use crate::engine::testkit::{seated, Player};
    use crate::engine::{apply_chain, replay, Chain, Table};
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn parse(line: &str) -> Result<Vec<Move>, String> {
        let tokens: Vec<String> = line.split_whitespace().map(String::from).collect();
        Farkle::parse_moves(&tokens)
    }

    #[test]
    fn keep_stops_eating_dice_at_the_next_verb() {
        assert_eq!(
            parse("keep 5 5 roll").unwrap(),
            vec![Move::Keep(vec![5, 5]), Move::Roll]
        );
        assert_eq!(
            parse("keep 1 bank").unwrap(),
            vec![Move::Keep(vec![1]), Move::Bank]
        );
        assert_eq!(
            parse("k 5 r").unwrap(),
            vec![Move::Keep(vec![5]), Move::Roll]
        );
        assert_eq!(
            parse("keep 1 keep 5").unwrap(),
            vec![Move::Keep(vec![1]), Move::Keep(vec![5])]
        );
    }

    #[test]
    fn single_moves_still_parse_alone() {
        assert_eq!(parse("roll").unwrap(), vec![Move::Roll]);
        assert_eq!(parse("bank").unwrap(), vec![Move::Bank]);
        assert_eq!(
            parse("keep 1 5 5").unwrap(),
            vec![Move::Keep(vec![1, 5, 5])]
        );
    }

    #[test]
    fn rejects_junk_without_swallowing_it() {
        assert!(parse("").is_err());
        assert!(parse("keep").unwrap_err().contains("keep needs dice"));
        assert!(parse("keep roll").unwrap_err().contains("keep needs dice"));
        assert!(parse("keep 7").unwrap_err().contains("not a die face"));
        assert!(parse("keep 5 wat").unwrap_err().contains("not a die face"));
        assert!(parse("yeet").unwrap_err().contains("unknown move"));
        assert!(parse("roll yeet").unwrap_err().contains("unknown move"));
    }

    /// The parser is a grammar, not a rules engine — legality is `fold`'s job.
    #[test]
    fn chains_are_parsed_even_when_illegal() {
        assert_eq!(parse("roll roll").unwrap(), vec![Move::Roll, Move::Roll]);
        assert_eq!(
            parse("bank keep 1").unwrap(),
            vec![Move::Bank, Move::Keep(vec![1])]
        );
    }

    #[test]
    fn events_serialize_without_derived_points() {
        assert_eq!(
            serde_json::to_string(&Event::Kept { dice: vec![1, 5] }).unwrap(),
            r#"{"kept":{"dice":[1,5]}}"#
        );
        assert_eq!(
            serde_json::to_string(&Event::Banked {}).unwrap(),
            r#"{"banked":{}}"#
        );
        assert!(serde_json::from_str::<Event>(r#"{"kept":{"dice":[1],"points":100}}"#).is_err());
    }

    /// Greedy bot: keep the highest-scoring valid subset of the current table.
    pub(crate) fn best_keep(table: &[u8], rules: &RuleSet) -> Vec<u8> {
        let mut best: (u32, Vec<u8>) = (0, vec![]);
        for mask in 1u32..(1 << table.len()) {
            let sel: Vec<u8> = (0..table.len())
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| table[i])
                .collect();
            if let Some(pts) = score_selection(&sel, rules) {
                if pts > best.0 {
                    best = (pts, sel);
                }
            }
        }
        best.1
    }

    /// The dice `plan` will roll for `n` dice from a fresh `seed`.
    pub(crate) fn roll_with(seed: u64, n: u8) -> Vec<u8> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..n).map(|_| rng.gen_range(1..=6)).collect()
    }

    /// First seed whose opening 6-dice roll satisfies `pred`.
    pub(crate) fn seed_where(rules: &RuleSet, pred: impl Fn(bool) -> bool) -> u64 {
        (0..)
            .find(|&s| pred(has_any_score(&roll_with(s, rules.dice_count), rules)))
            .unwrap()
    }

    fn play(
        s: &mut Session,
        t: &mut Table<Farkle>,
        players: &mut [Player],
        seat: usize,
        moves: &[Move],
    ) -> Chain<Farkle> {
        let p = &mut players[seat];
        apply_chain(s, t, seat, moves, &mut p.ctx, &p.sk)
    }

    /// Drive a full 2-player game to completion with a seeded RNG and a greedy
    /// strategy, through `apply_chain`. The final `replay` of the signed log
    /// must equal the live state.
    #[test]
    fn plays_a_full_game_to_a_win() {
        let (mut s, mut t, mut players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        players[0].ctx.rng = StdRng::seed_from_u64(42);
        players[1].ctx.rng = StdRng::seed_from_u64(43);

        let mut guard = 0;
        while Farkle::winner(&t.state).is_none() {
            guard += 1;
            assert!(guard < 100_000, "game failed to terminate");
            let st = &t.state;
            let mv = match st.phase {
                Phase::AwaitRoll => Move::Roll,
                Phase::AwaitKeep => Move::Keep(best_keep(&st.table, &st.rules)),
                Phase::AwaitRollOrBank => {
                    if st.turn_total >= 300 || st.dice_available <= 2 {
                        Move::Bank
                    } else {
                        Move::Roll
                    }
                }
                Phase::GameOver => unreachable!(),
            };
            let seat = Farkle::to_move(st).unwrap();
            let chain = play(&mut s, &mut t, &mut players, seat, &[mv]);
            assert_eq!(chain.failure, None);
            assert_eq!(chain.steps.len(), 1);
        }

        let winner = t.state.winner.expect("someone won");
        assert!(t.state.totals[winner] >= t.state.rules.target_score);
        let replayed = replay::<Farkle>(&s).expect("the signed log verifies");
        assert_eq!(replayed.state, t.state);
        assert_eq!(replayed.head, t.head);
    }

    #[test]
    fn chain_legal_prefix_persists() {
        let rules = RuleSet::kcd();
        let seed = seed_where(&rules, |scores| scores);
        let (mut s, mut t, mut players) = seated::<Farkle>(&rules, &["Alice", "Bob"]);
        players[0].ctx.rng = StdRng::seed_from_u64(seed);
        let len = s.log.len();

        let chain = play(
            &mut s,
            &mut t,
            &mut players,
            0,
            &[Move::Roll, Move::Keep(vec![6; 7]), Move::Bank],
        );
        let table = roll_with(seed, 6);
        assert_eq!(
            chain.failure.as_deref(),
            Some(
                format!(
                    "those dice aren't all on the table (table: {})",
                    fmt_dice(&table)
                )
                .as_str()
            )
        );
        assert_eq!(chain.steps.len(), 1);
        assert_eq!(s.log.len(), len + 1);
        assert_eq!(t.state.table, table);
        assert_eq!(replay::<Farkle>(&s).unwrap().state, t.state);
    }

    #[test]
    fn chain_halts_at_the_turn_boundary() {
        let rules = RuleSet::kcd();
        let seed = seed_where(&rules, |scores| scores);
        let (mut s, mut t, mut players) = seated::<Farkle>(&rules, &["Alice", "Bob"]);
        players[0].ctx.rng = StdRng::seed_from_u64(seed);
        let keep = best_keep(&roll_with(seed, 6), &rules);

        let chain = play(
            &mut s,
            &mut t,
            &mut players,
            0,
            &[Move::Roll, Move::Keep(keep), Move::Bank, Move::Roll],
        );
        assert_eq!(chain.failure.as_deref(), Some("it's Bob's turn, not yours"));
        assert_eq!(chain.steps.len(), 3);
        assert_eq!(Farkle::to_move(&t.state), Some(1));
        assert!(t.state.totals[0] > 0);
    }

    #[test]
    fn chain_bust_halts() {
        let rules = RuleSet::kcd();
        let seed = seed_where(&rules, |scores| !scores);
        let (mut s, mut t, mut players) = seated::<Farkle>(&rules, &["Alice", "Bob"]);
        players[0].ctx.rng = StdRng::seed_from_u64(seed);

        let chain = play(&mut s, &mut t, &mut players, 0, &[Move::Roll, Move::Roll]);
        assert_eq!(chain.failure.as_deref(), Some("it's Bob's turn, not yours"));
        assert_eq!(chain.steps.len(), 1);
        let step = &chain.steps[0];
        let names = t.names.clone();
        let lines = Farkle::narrate(&step.before, 0, &step.event, &step.after, &names);
        assert_eq!(lines[1], "BUST! No scoring dice. Alice loses this turn.");
        assert_eq!(t.state.totals, vec![0, 0]);
    }

    /// Real, seeded play (roll, best keep, bank) until the seat to move will
    /// win with the best keep of its next roll. Returns that seat and keep.
    pub(crate) fn one_keep_from_a_win() -> (Session, Table<Farkle>, Vec<Player>, usize, Vec<u8>) {
        let rules = RuleSet::kcd();
        let (mut s, mut t, mut players) = seated::<Farkle>(&rules, &["Alice", "Bob"]);
        for _ in 0..10_000 {
            let st = &t.state;
            let seat = Farkle::to_move(st).expect("nobody has won yet");
            let mut peek = players[seat].ctx.rng.clone();
            let roll: Vec<u8> = (0..st.dice_available)
                .map(|_| peek.gen_range(1..=6))
                .collect();
            let keep = best_keep(&roll, &rules);
            let turn = st.turn_total + score_selection(&keep, &rules).unwrap_or(0);
            let can_bank = st.totals[seat] > 0 || turn >= rules.on_board_minimum;
            if !keep.is_empty() && can_bank && st.totals[seat] + turn >= rules.target_score {
                return (s, t, players, seat, keep);
            }
            let moves = match (keep.is_empty(), can_bank) {
                (true, _) => vec![Move::Roll],
                (false, true) => vec![Move::Roll, Move::Keep(keep), Move::Bank],
                (false, false) => vec![Move::Roll, Move::Keep(keep)],
            };
            assert_eq!(
                play(&mut s, &mut t, &mut players, seat, &moves).failure,
                None
            );
        }
        panic!("no win within 10000 turns");
    }

    /// The win comes from real signed moves, so the log replays to it (§13).
    #[test]
    fn chain_win_halts_and_reports() {
        let (mut s, mut t, mut players, seat, keep) = one_keep_from_a_win();
        let chain = play(
            &mut s,
            &mut t,
            &mut players,
            seat,
            &[Move::Roll, Move::Keep(keep), Move::Bank, Move::Roll],
        );
        assert_eq!(chain.failure.as_deref(), Some("the game is over"));
        assert_eq!(chain.steps.len(), 3);
        assert_eq!(Farkle::winner(&t.state), Some(seat));
        let name = &t.names[seat];
        assert!(Farkle::render(&t.state, &t.names, Some(seat), None)
            .ends_with(&format!(">>> {name} WINS! <<<")));
        assert_eq!(
            Farkle::game_over(&t.state, &t.names),
            Some(format!("Game over — {name} won."))
        );
        let replayed = replay::<Farkle>(&s).expect("the signed log verifies");
        assert_eq!(replayed.state, t.state);
        assert_eq!(replayed.head, t.head);
    }

    #[test]
    fn chain_with_no_moves_is_status_only() {
        let (mut s, mut t, mut players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        let len = s.log.len();
        let chain = play(&mut s, &mut t, &mut players, 1, &[]);
        assert_eq!(chain.failure, None);
        assert!(chain.steps.is_empty());
        assert_eq!(s.log.len(), len);
    }

    #[test]
    fn hints_name_the_new_cli() {
        let (_, t, _) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        assert!(Farkle::render(&t.state, &t.names, None, None)
            .ends_with("Alice's turn — roll: zs play roll"));
    }
}
