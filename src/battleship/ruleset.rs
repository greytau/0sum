//! Declarative Battleship rules. Pure data; mechanics read every variant from here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShipKind {
    pub name: String,
    pub len: u8,
}

/// Stored in genesis so every session is self-describing and reproducible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub name: String,
    /// At most 26 (row letters).
    pub rows: u8,
    /// At most 26.
    pub cols: u8,
    /// Fleet order is slot order (ship index k).
    pub fleet: Vec<ShipKind>,
    /// Ships may share an edge or corner. When false, no two ships are 8-adjacent.
    pub touching: bool,
}

impl RuleSet {
    pub fn by_name(name: &str) -> Result<RuleSet, String> {
        match name.to_ascii_lowercase().as_str() {
            "classic" => Ok(RuleSet::classic()),
            "notouch" => Ok(RuleSet::notouch()),
            other => Err(format!(
                "unknown ruleset `{other}` (available: classic, notouch)"
            )),
        }
    }

    /// The default. 10×10, carrier 5, battleship 4, cruiser 3, submarine 3,
    /// destroyer 2; touching allowed.
    pub fn classic() -> RuleSet {
        let ship = |name: &str, len| ShipKind {
            name: name.into(),
            len,
        };
        RuleSet {
            name: "classic".into(),
            rows: 10,
            cols: 10,
            fleet: vec![
                ship("carrier", 5),
                ship("battleship", 4),
                ship("cruiser", 3),
                ship("submarine", 3),
                ship("destroyer", 2),
            ],
            touching: true,
        }
    }

    /// Same fleet, ships may not touch, not even diagonally.
    pub fn notouch() -> RuleSet {
        RuleSet {
            name: "notouch".into(),
            touching: false,
            ..RuleSet::classic()
        }
    }

    pub fn fleet_cells(&self) -> u16 {
        self.fleet.iter().map(|s| s.len as u16).sum()
    }

    pub fn cells(&self) -> u16 {
        self.rows as u16 * self.cols as u16
    }
}
