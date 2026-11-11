//! Declarative rule configuration (issue #2).
//!
//! A `RuleSet` is pure data the game mechanics consume. Scoring reads every
//! value from here — there are no hardcoded scoring numbers in the engine.
//! Swapping the ruleset swaps the whole scoring behaviour without touching
//! mechanics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A full set of Farkle rules. Stored in the session genesis block so every
/// session is self-describing and reproducible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub name: String,
    pub dice_count: u8,

    /// Faces that score as singles: face -> points (KCD: only 1s and 5s).
    pub singles: BTreeMap<u8, u32>,

    /// Base value of a three-of-a-kind per face (1s special-cased to 1000).
    pub three_of_a_kind: BTreeMap<u8, u32>,

    /// Multiplier applied to the triple base for n-of-a-kind.
    /// KCD quirk: {3:1, 4:2, 5:4, 6:8} — each extra die *doubles* the triple.
    pub n_of_a_kind_multiplier: BTreeMap<u8, u32>,

    /// Straights: the exact dice faces that must be present -> points.
    /// Note KCD's asymmetry: 1-2-3-4-5 = 500 but 2-3-4-5-6 = 750.
    pub straights: Vec<(Vec<u8>, u32)>,

    /// Three-pairs combo value (consumes all 6). `None` = does not score (KCD).
    pub three_pairs: Option<u32>,

    /// Two-triplets combo value (consumes all 6). `None` = does not score (KCD).
    pub two_triplets: Option<u32>,

    /// Minimum turn total required to first get "on the board" (KCD: 0).
    pub on_board_minimum: u32,

    /// Score that ends the game. A parameter, not a constant — kautau can tweak.
    pub target_score: u32,
}

impl RuleSet {
    /// Resolve a ruleset by name for the `--ruleset` flag.
    pub fn by_name(name: &str) -> Result<RuleSet, String> {
        match name.to_ascii_lowercase().as_str() {
            "kcd" => Ok(RuleSet::kcd()),
            "classic" => Ok(RuleSet::classic()),
            other => Err(format!(
                "unknown ruleset `{other}` (available: kcd, classic)"
            )),
        }
    }

    /// Kingdom Come: Deliverance "Dice" — the default.
    pub fn kcd() -> RuleSet {
        RuleSet {
            name: "kcd".into(),
            dice_count: 6,
            singles: BTreeMap::from([(1, 100), (5, 50)]),
            three_of_a_kind: BTreeMap::from([
                (1, 1000),
                (2, 200),
                (3, 300),
                (4, 400),
                (5, 500),
                (6, 600),
            ]),
            n_of_a_kind_multiplier: BTreeMap::from([(3, 1), (4, 2), (5, 4), (6, 8)]),
            straights: vec![
                (vec![1, 2, 3, 4, 5], 500),
                (vec![2, 3, 4, 5, 6], 750),
                (vec![1, 2, 3, 4, 5, 6], 1500),
            ],
            three_pairs: None,
            two_triplets: None,
            on_board_minimum: 0,
            target_score: 4000,
        }
    }

    /// A minimal "classic" Farkle variant — deliberately different from KCD to
    /// prove the ruleset abstraction (issue #2 acceptance): it scores combos
    /// KCD does not (three pairs, two triplets), only scores the *full*
    /// straight, and enforces an on-the-board minimum.
    pub fn classic() -> RuleSet {
        RuleSet {
            name: "classic".into(),
            dice_count: 6,
            singles: BTreeMap::from([(1, 100), (5, 50)]),
            three_of_a_kind: BTreeMap::from([
                (1, 1000),
                (2, 200),
                (3, 300),
                (4, 400),
                (5, 500),
                (6, 600),
            ]),
            n_of_a_kind_multiplier: BTreeMap::from([(3, 1), (4, 2), (5, 4), (6, 8)]),
            straights: vec![(vec![1, 2, 3, 4, 5, 6], 1500)],
            three_pairs: Some(1500),
            two_triplets: Some(2500),
            on_board_minimum: 500,
            target_score: 10000,
        }
    }
}
