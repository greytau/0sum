//! Declarative Liar's Dice rules. Pure data; mechanics read every variant from here.

use serde::{Deserialize, Serialize};

/// Stored in genesis so every session is self-describing and reproducible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub name: String,
    /// Dice each player starts with.
    pub dice_per_player: u8,
    /// 1s count toward every bid face, and 1s cannot themselves be bid.
    /// When false, 1s are an ordinary face (the lowest) and can be bid.
    pub wild_ones: bool,
    /// Enables the `spot` call: claim the last bid is EXACTLY right.
    pub spot_on: bool,
}

impl RuleSet {
    pub fn by_name(name: &str) -> Result<RuleSet, String> {
        match name.to_ascii_lowercase().as_str() {
            "classic" => Ok(RuleSet::classic()),
            "natural" => Ok(RuleSet::natural()),
            other => Err(format!(
                "unknown ruleset `{other}` (available: classic, natural)"
            )),
        }
    }

    /// The default. Wild 1s, no spot-on.
    pub fn classic() -> RuleSet {
        RuleSet {
            name: "classic".into(),
            dice_per_player: 5,
            wild_ones: true,
            spot_on: false,
        }
    }

    /// Proves the abstraction: no wild 1s (1s biddable as the lowest face),
    /// spot-on enabled.
    pub fn natural() -> RuleSet {
        RuleSet {
            name: "natural".into(),
            dice_per_player: 5,
            wild_ones: false,
            spot_on: true,
        }
    }

    pub fn lowest_bid_face(&self) -> u8 {
        if self.wild_ones {
            2
        } else {
            1
        }
    }
}
