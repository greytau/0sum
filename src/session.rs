//! The session state model: an append-only log, with all game state derived as
//! a projection (fold) over that log.
//!
//! Nothing mutable — no score field, no "whose turn" field — is stored. The
//! file is `{ version, session_id, log: [...] }`; the genesis entry carries the
//! game + ruleset + roster, and every later entry is a move. Scores, turn
//! order, the current roll and registration are all *computed* by [`project`].
//!
//! Phase 1 has no crypto. The `LogEntry` envelope (seq + timestamp + author +
//! payload) is deliberately the append unit so the later signed-hash-chain
//! chunk can add `prev_hash` / `signature` fields to the envelope without
//! reshaping payloads or the projection.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ruleset::RuleSet;

/// One append-only entry. The envelope the future hash-chain will wrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub ts: u64,
    /// Player this entry is attributed to; `None` for system events (genesis).
    pub author: Option<String>,
    pub payload: Payload,
}

/// A roster seat, declared at `new` time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterEntry {
    pub player_id: String,
    pub name: String,
}

/// The event types. Score is never one of them — it is projected from these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    /// Genesis. Always `log[0]`.
    SessionCreated {
        game: String,
        ruleset: RuleSet,
        roster: Vec<RosterEntry>,
    },
    PlayerRegistered {
        player_id: String,
    },
    Rolled {
        dice: Vec<u8>,
    },
    Kept {
        dice: Vec<u8>,
        points: u32,
    },
    Banked {
        points: u32,
    },
    Busted {
        forfeited: u32,
    },
}

/// The on-disk session file. Self-contained and portable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub session_id: String,
    pub log: Vec<LogEntry>,
}

impl Session {
    pub fn new(session_id: String, genesis: Payload) -> Session {
        Session {
            version: 1,
            session_id,
            log: vec![LogEntry {
                seq: 0,
                ts: now(),
                author: None,
                payload: genesis,
            }],
        }
    }

    /// Append a payload attributed to `author`, assigning the next seq + ts.
    pub fn append(&mut self, author: Option<String>, payload: Payload) {
        self.log.push(LogEntry {
            seq: self.log.len() as u64,
            ts: now(),
            author,
            payload,
        });
    }

    /// Fold the log into current game state.
    pub fn project(&self) -> GameState {
        let (game, ruleset, roster) = match &self.log[0].payload {
            Payload::SessionCreated {
                game,
                ruleset,
                roster,
            } => (game.clone(), ruleset.clone(), roster.clone()),
            _ => panic!("corrupt session: genesis is not SessionCreated"),
        };

        let seats = roster.len();
        let mut totals: BTreeMap<String, u32> =
            roster.iter().map(|r| (r.player_id.clone(), 0)).collect();
        let mut registered: BTreeSet<String> = BTreeSet::new();
        let mut turn_index = 0usize;
        let mut winner: Option<String> = None;

        // Current-turn accumulators.
        let mut turn_total = 0u32;
        let mut dice_available = ruleset.dice_count;
        let mut table: Vec<u8> = Vec::new();
        let mut phase = Phase::AwaitRoll;

        for entry in &self.log[1..] {
            match &entry.payload {
                Payload::PlayerRegistered { player_id } => {
                    registered.insert(player_id.clone());
                }
                Payload::Rolled { dice } => {
                    table = dice.clone();
                    phase = Phase::AwaitKeep;
                }
                Payload::Kept { dice, points } => {
                    turn_total += points;
                    dice_available = dice_available.saturating_sub(dice.len() as u8);
                    table.clear();
                    if dice_available == 0 {
                        dice_available = ruleset.dice_count; // hot dice
                    }
                    phase = Phase::AwaitRollOrBank;
                }
                Payload::Banked { points } => {
                    let cur = roster[turn_index].player_id.clone();
                    *totals.get_mut(&cur).unwrap() += points;
                    if totals[&cur] >= ruleset.target_score {
                        winner = Some(cur);
                    }
                    turn_index = (turn_index + 1) % seats;
                    turn_total = 0;
                    dice_available = ruleset.dice_count;
                    table.clear();
                    phase = Phase::AwaitRoll;
                }
                Payload::Busted { .. } => {
                    turn_index = (turn_index + 1) % seats;
                    turn_total = 0;
                    dice_available = ruleset.dice_count;
                    table.clear();
                    phase = Phase::AwaitRoll;
                }
                Payload::SessionCreated { .. } => {}
            }
        }

        let started = registered.len() == seats;
        if winner.is_some() {
            phase = Phase::GameOver;
        } else if !started {
            phase = Phase::AwaitRoll;
        }

        GameState {
            game,
            ruleset,
            roster,
            registered,
            totals,
            turn_index,
            started,
            winner,
            turn_total,
            dice_available,
            table,
            phase,
        }
    }
}

/// What action the current player may take next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    AwaitRoll,
    AwaitKeep,
    AwaitRollOrBank,
    GameOver,
}

/// The projection: everything derived from the log.
#[derive(Debug, Clone)]
pub struct GameState {
    pub game: String,
    pub ruleset: RuleSet,
    pub roster: Vec<RosterEntry>,
    pub registered: BTreeSet<String>,
    pub totals: BTreeMap<String, u32>,
    pub turn_index: usize,
    pub started: bool,
    pub winner: Option<String>,
    pub turn_total: u32,
    pub dice_available: u8,
    pub table: Vec<u8>,
    pub phase: Phase,
}

impl GameState {
    pub fn current_player(&self) -> &RosterEntry {
        &self.roster[self.turn_index]
    }

    pub fn name_of(&self, player_id: &str) -> &str {
        self.roster
            .iter()
            .find(|r| r.player_id == player_id)
            .map(|r| r.name.as_str())
            .unwrap_or("?")
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
