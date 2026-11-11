//! Game mechanics (issue #2): the stable turn machinery — rolling, hot-dice,
//! bust, keep-validation, banking. Mechanics never hardcode scoring; they defer
//! every value question to the [`RuleSet`] via [`crate::scoring`].
//!
//! The `Game` trait keeps mechanics decoupled from rules so future games slot
//! in as new impls. Farkle is impl #1.

use rand::rngs::StdRng;
use rand::Rng;

use crate::scoring::{has_any_score, score_selection};
use crate::session::{GameState, Payload, Phase};

/// A parsed player action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Roll,
    Keep(Vec<u8>),
    Bank,
}

pub trait Game {
    fn id(&self) -> &'static str;

    /// Parse the CLI move tokens into the sequence of [`Move`]s they spell out.
    /// Several moves may be chained in one invocation (`keep 5 5 roll`).
    fn parse_moves(&self, tokens: &[String]) -> Result<Vec<Move>, String>;

    /// Apply a move against projected state, producing the events to append.
    /// Does not mutate — it returns payloads for the caller to append+persist.
    fn apply(&self, state: &GameState, mv: &Move, rng: &mut StdRng)
        -> Result<Vec<Payload>, String>;
}

pub struct Farkle;

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
    fn id(&self) -> &'static str {
        "farkle"
    }

    fn parse_moves(&self, tokens: &[String]) -> Result<Vec<Move>, String> {
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

    fn apply(
        &self,
        state: &GameState,
        mv: &Move,
        rng: &mut StdRng,
    ) -> Result<Vec<Payload>, String> {
        match mv {
            Move::Roll => match state.phase {
                Phase::AwaitRoll | Phase::AwaitRollOrBank => {
                    let dice: Vec<u8> = (0..state.dice_available)
                        .map(|_| rng.gen_range(1..=6))
                        .collect();
                    if has_any_score(&dice, &state.ruleset) {
                        Ok(vec![Payload::Rolled { dice }])
                    } else {
                        Ok(vec![
                            Payload::Rolled { dice },
                            Payload::Busted {
                                forfeited: state.turn_total,
                            },
                        ])
                    }
                }
                Phase::AwaitKeep => {
                    Err("you have dice on the table — `keep` scoring dice first".into())
                }
                Phase::GameOver => Err("the game is over".into()),
            },

            Move::Keep(sel) => {
                if state.phase != Phase::AwaitKeep {
                    return Err("nothing to keep — `roll` first".into());
                }
                if !is_submultiset(sel, &state.table) {
                    return Err(format!(
                        "those dice aren't all on the table (table: {:?})",
                        state.table
                    ));
                }
                match score_selection(sel, &state.ruleset) {
                    Some(points) if points > 0 => Ok(vec![Payload::Kept {
                        dice: sel.clone(),
                        points,
                    }]),
                    _ => Err(
                        "that isn't a valid scoring selection (every kept die must score)".into(),
                    ),
                }
            }

            Move::Bank => {
                if state.phase != Phase::AwaitRollOrBank {
                    return Err("you can only bank after keeping scoring dice".into());
                }
                let cur = &state.current_player().player_id;
                let on_board = state.totals[cur] > 0;
                if !on_board && state.turn_total < state.ruleset.on_board_minimum {
                    return Err(format!(
                        "you need at least {} in a turn to get on the board (have {})",
                        state.ruleset.on_board_minimum, state.turn_total
                    ));
                }
                Ok(vec![Payload::Banked {
                    points: state.turn_total,
                }])
            }
        }
    }
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
mod tests {
    use super::*;
    use crate::ruleset::RuleSet;
    use crate::session::{Payload, RosterEntry, Session};
    use rand::SeedableRng;

    fn two_player_session(ruleset: RuleSet) -> Session {
        let roster = vec![
            RosterEntry {
                player_id: "p1".into(),
                name: "Alice".into(),
            },
            RosterEntry {
                player_id: "p2".into(),
                name: "Bob".into(),
            },
        ];
        let mut s = Session::new(
            "TEST01".into(),
            Payload::SessionCreated {
                game: "farkle".into(),
                ruleset,
                roster,
            },
        );
        s.append(
            Some("p1".into()),
            Payload::PlayerRegistered {
                player_id: "p1".into(),
            },
        );
        s.append(
            Some("p2".into()),
            Payload::PlayerRegistered {
                player_id: "p2".into(),
            },
        );
        s
    }

    fn parse(line: &str) -> Result<Vec<Move>, String> {
        let tokens: Vec<String> = line.split_whitespace().map(String::from).collect();
        Farkle.parse_moves(&tokens)
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

    /// The parser is a grammar, not a rules engine — legality is `apply`'s job.
    #[test]
    fn chains_are_parsed_even_when_illegal() {
        assert_eq!(parse("roll roll").unwrap(), vec![Move::Roll, Move::Roll]);
        assert_eq!(
            parse("bank keep 1").unwrap(),
            vec![Move::Bank, Move::Keep(vec![1])]
        );
    }

    /// Greedy bot: keep the highest-scoring valid subset of the current table.
    fn best_keep(table: &[u8], rules: &RuleSet) -> Vec<u8> {
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

    /// Drive a full 2-player game to completion with a seeded RNG and a greedy
    /// strategy. Proves the whole turn machinery — roll, keep, hot-dice, bust,
    /// bank, win — actually reaches a winner and that banked totals are
    /// consistent with the projection. Deterministic via the seed.
    #[test]
    fn plays_a_full_game_to_a_win() {
        let farkle = Farkle;
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let mut session = two_player_session(RuleSet::kcd());

        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 100_000, "game failed to terminate");

            let state = session.project();
            if state.winner.is_some() {
                break;
            }
            assert!(state.started);

            let mv = match state.phase {
                Phase::AwaitRoll => Move::Roll,
                Phase::AwaitKeep => Move::Keep(best_keep(&state.table, &state.ruleset)),
                Phase::AwaitRollOrBank => {
                    // Bank once the turn is worth a bit or the dice run thin.
                    if state.turn_total >= 300 || state.dice_available <= 2 {
                        Move::Bank
                    } else {
                        Move::Roll
                    }
                }
                Phase::GameOver => unreachable!(),
            };

            let author = state.current_player().player_id.clone();
            let payloads = farkle.apply(&state, &mv, &mut rng).expect("legal move");
            for p in payloads {
                session.append(Some(author.clone()), p);
            }
        }

        let final_state = session.project();
        let winner = final_state.winner.expect("someone won");
        assert!(final_state.totals[&winner] >= final_state.ruleset.target_score);

        // The winner's total must equal the sum of their Banked events (score is
        // purely a projection of the log, nothing stored).
        let banked_by_winner: u32 = winner_bank_total(&session, &winner);
        assert_eq!(final_state.totals[&winner], banked_by_winner);
    }

    fn winner_bank_total(session: &Session, winner: &str) -> u32 {
        session
            .log
            .iter()
            .filter(|e| e.author.as_deref() == Some(winner))
            .filter_map(|e| match &e.payload {
                Payload::Banked { points } => Some(*points),
                _ => None,
            })
            .sum()
    }
}
