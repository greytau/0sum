//! Farkle scoring — reads every value from the active [`RuleSet`]. No scoring
//! constant lives here.
//!
//! Two questions the engine asks:
//!   - [`has_any_score`]: does a roll contain *any* scoring die? (bust check)
//!   - [`score_selection`]: can a chosen set of dice be *fully* partitioned into
//!     scoring groups, and what is the best total? (keep validation)

use crate::ruleset::RuleSet;

/// Face counts indexed 1..=6 (index 0 unused).
type Counts = [u8; 7];

fn counts_of(dice: &[u8]) -> Counts {
    let mut c = [0u8; 7];
    for &d in dice {
        if (1..=6).contains(&d) {
            c[d as usize] += 1;
        }
    }
    c
}

fn total(c: &Counts) -> u8 {
    c[1..=6].iter().sum()
}

fn has_straight(c: &Counts, pattern: &[u8]) -> bool {
    pattern.iter().all(|&f| c[f as usize] >= 1)
}

/// Exactly six dice forming pairs only (2+2+2, 4+2, or 6-of-a-kind).
fn is_three_pairs(c: &Counts) -> bool {
    total(c) == 6 && (1..=6).all(|f| c[f].is_multiple_of(2))
}

/// Exactly six dice forming two triplets (3+3 or a single 6-of-a-kind).
fn is_two_triplets(c: &Counts) -> bool {
    total(c) == 6 && (1..=6).all(|f| c[f] == 0 || c[f] == 3 || c[f] == 6)
}

/// True if the roll contains at least one scoring opportunity. A roll with
/// none is a bust.
pub fn has_any_score(dice: &[u8], rules: &RuleSet) -> bool {
    let c = counts_of(dice);
    if rules.singles.keys().any(|&f| c[f as usize] > 0) {
        return true;
    }
    if (1..=6).any(|f| c[f] >= 3) {
        return true;
    }
    if rules.straights.iter().any(|(p, _)| has_straight(&c, p)) {
        return true;
    }
    if rules.three_pairs.is_some() && is_three_pairs(&c) {
        return true;
    }
    if rules.two_triplets.is_some() && is_two_triplets(&c) {
        return true;
    }
    false
}

/// Best score for a selection that must be *entirely* consumed by scoring
/// groups. `None` if any die can't be part of a scoring group (an invalid
/// keep). This is the max over every legal decomposition.
pub fn score_selection(dice: &[u8], rules: &RuleSet) -> Option<u32> {
    if dice.is_empty() {
        return None;
    }
    best_decomposition(counts_of(dice), rules)
}

/// Recursively peel one scoring group, recurse on the remainder, keep the max.
/// Returns `Some(points)` only if the dice fully decompose into scoring groups.
fn best_decomposition(c: Counts, rules: &RuleSet) -> Option<u32> {
    if total(&c) == 0 {
        return Some(0);
    }
    let mut best: Option<u32> = None;
    let consider = |pts: u32, rest: Counts, best: &mut Option<u32>| {
        if let Some(r) = best_decomposition(rest, rules) {
            let candidate = pts + r;
            *best = Some(best.map_or(candidate, |b| b.max(candidate)));
        }
    };

    // Straights (each consumes one of every face in the pattern).
    for (pattern, pts) in &rules.straights {
        if has_straight(&c, pattern) {
            let mut rest = c;
            for &f in pattern {
                rest[f as usize] -= 1;
            }
            consider(*pts, rest, &mut best);
        }
    }

    // Whole-hand combos (consume all six).
    if let Some(pts) = rules.three_pairs {
        if is_three_pairs(&c) {
            consider(pts, [0; 7], &mut best);
        }
    }
    if let Some(pts) = rules.two_triplets {
        if is_two_triplets(&c) {
            consider(pts, [0; 7], &mut best);
        }
    }

    // N-of-a-kind (n from 3 up to however many we have).
    for f in 1..=6u8 {
        let count = c[f as usize];
        if count < 3 {
            continue;
        }
        for take in 3..=count {
            if let (Some(&base), Some(&mult)) = (
                rules.three_of_a_kind.get(&f),
                rules.n_of_a_kind_multiplier.get(&take),
            ) {
                let mut rest = c;
                rest[f as usize] -= take;
                consider(base * mult, rest, &mut best);
            }
        }
    }

    // Singles.
    for (&face, &pts) in &rules.singles {
        if c[face as usize] >= 1 {
            let mut rest = c;
            rest[face as usize] -= 1;
            consider(pts, rest, &mut best);
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ruleset::RuleSet;

    fn kcd() -> RuleSet {
        RuleSet::kcd()
    }

    #[test]
    fn singles_score_from_ruleset() {
        assert_eq!(score_selection(&[1], &kcd()), Some(100));
        assert_eq!(score_selection(&[5], &kcd()), Some(50));
        assert_eq!(score_selection(&[1, 5], &kcd()), Some(150));
    }

    #[test]
    fn non_scoring_dice_are_invalid_keeps() {
        // A single 2 can't be kept.
        assert_eq!(score_selection(&[2], &kcd()), None);
        // Mixed valid+invalid is still invalid (must fully decompose).
        assert_eq!(score_selection(&[1, 2], &kcd()), None);
    }

    #[test]
    fn bust_detection() {
        assert!(!has_any_score(&[2, 3, 4, 6], &kcd()));
        assert!(has_any_score(&[2, 3, 4, 5], &kcd())); // the 5 scores
    }

    #[test]
    fn triple_uses_face_value_not_singles() {
        // Three 5s = 500 (the triple), not 3×50.
        assert_eq!(score_selection(&[5, 5, 5], &kcd()), Some(500));
        // Three 1s special-cased to 1000.
        assert_eq!(score_selection(&[1, 1, 1], &kcd()), Some(1000));
    }

    #[test]
    fn kcd_doubling_quirk() {
        // 4/5/6-of-a-kind double the triple base repeatedly.
        assert_eq!(score_selection(&[2, 2, 2, 2], &kcd()), Some(400)); // 200 * 2
        assert_eq!(score_selection(&[5, 5, 5, 5, 5], &kcd()), Some(2000)); // 500 * 4
        assert_eq!(
            score_selection(&[1, 1, 1, 1, 1, 1], &kcd()),
            Some(8000) // 1000 * 8
        );
        // Four 1s as a group (2000) beats triple+single (1100).
        assert_eq!(score_selection(&[1, 1, 1, 1], &kcd()), Some(2000));
    }

    #[test]
    fn asymmetric_straights() {
        assert_eq!(score_selection(&[1, 2, 3, 4, 5], &kcd()), Some(500));
        assert_eq!(score_selection(&[2, 3, 4, 5, 6], &kcd()), Some(750));
        assert_eq!(score_selection(&[1, 2, 3, 4, 5, 6], &kcd()), Some(1500));
    }

    #[test]
    fn full_straight_beats_partial_plus_single() {
        // 1-6 could be scored as 2-3-4-5-6 (750) + single 1 (100) = 850,
        // but the full straight (1500) must win.
        assert_eq!(score_selection(&[1, 2, 3, 4, 5, 6], &kcd()), Some(1500));
    }

    #[test]
    fn kcd_does_not_score_three_pairs_or_two_triplets() {
        // Three pairs of non-single faces don't decompose in KCD.
        assert_eq!(score_selection(&[2, 2, 3, 3, 4, 4], &kcd()), None);
        assert!(!has_any_score(&[2, 2, 3, 3, 4, 4], &kcd()));
    }

    #[test]
    fn classic_scores_three_pairs_and_two_triplets() {
        let c = RuleSet::classic();
        assert_eq!(score_selection(&[2, 2, 3, 3, 4, 4], &c), Some(1500));
        // Two triplets (2500) beats two separate triples (200 + 300 = 500).
        assert_eq!(score_selection(&[2, 2, 2, 3, 3, 3], &c), Some(2500));
        // Classic only scores the full straight, not partials.
        assert_eq!(score_selection(&[1, 2, 3, 4, 5], &c), None);
        assert_eq!(score_selection(&[1, 2, 3, 4, 5, 6], &c), Some(1500));
    }
}
