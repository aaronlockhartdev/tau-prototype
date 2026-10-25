//! The single sequence-alignment primitive behind anchor re-use (#298): which
//! previous lines a new file's lines pair with. All three re-anchor sites —
//! the pure edit path (`apply_edits`), the registry's re-read
//! (`Registry::allocate`) and the pin store's re-anchor
//! ([`pin_store::reanchor`](super::pin_store::reanchor)) — call [`align`], so
//! a line the session owns survives (or does not) identically everywhere.
//!
//! The pairing is a **longest common subsequence** of the two checksum
//! sequences: no larger in-order matching exists. The edit path used a greedy
//! cursor walk instead, which pairs fewer lines than the maximal subsequence:
//! on 200,000 random checksum pairs drawn in the shape the proptests below
//! generate, the walk came out the shorter pairing about a quarter of the
//! time, dropping 1.15 survivors per miss. The registry already ran the
//! longest matching, so the same survivor kept its anchor on re-read and lost
//! it on edit.
//!
//! Where several longest pairings exist, which one `align` returns is its own
//! choice: the caller keys an anchor to whatever slot the pairing lands on, so
//! only the *count* carried is contractual. Callers must not depend on a
//! particular longest pairing.

/// The in-order pairing of two line-checksum sequences: for each index of
/// `new_checksums`, the index of `prev_checksums` it pairs with, or `None`
/// where the line is fresh. Every pair has equal checksums, the pairs are
/// strictly increasing in both sequences, and their count is the length of the
/// longest common subsequence (#298).
///
/// The pairing, not a per-checksum lookup, is what keeps repeated lines
/// unique: two new copies of one old checksum pair with two *distinct*
/// previous slots.
#[must_use]
pub(crate) fn align(prev_checksums: &[String], new_checksums: &[String]) -> Vec<Option<usize>> {
    let n = prev_checksums.len();
    let m = new_checksums.len();
    if n == 0 || m == 0 {
        return vec![None; m];
    }
    // `table[i][j]` is the longest common subsequence length of the two
    // suffixes starting at `i` and `j`; the walk below reads it to decide
    // which side to skip.
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if prev_checksums[i] == new_checksums[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut paired: Vec<Option<usize>> = vec![None; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if prev_checksums[i] == new_checksums[j] {
            paired[j] = Some(i);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    paired
}

/// The longest common subsequence length by enumeration over the index subsets
/// of both sequences — deliberately not an LCS algorithm, so the maximality
/// claim cannot validate itself through the DP `align` runs (#298). Test-only,
/// and shared with [`properties`](super::properties) so the edit path, the
/// registry and the pin store are measured against the same yardstick.
#[cfg(test)]
pub(crate) fn brute_force_lcs(a: &[String], b: &[String]) -> usize {
    let pick = |seq: &[String], mask: usize| -> Vec<String> {
        seq.iter()
            .enumerate()
            .filter(|(k, _)| mask & (1 << k) != 0)
            .map(|(_, s)| s.clone())
            .collect()
    };
    let mut best = 0usize;
    for ma in 0..(1usize << a.len()) {
        let ea = pick(a, ma);
        if ea.len() <= best || !(0..(1usize << b.len())).any(|mb| ea == pick(b, mb)) {
            continue;
        }
        best = ea.len();
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A pairing is usable: strictly increasing on both sides, and every pair
    /// joins equal checksums.
    fn is_valid_pairing(prev: &[String], next: &[String], paired: &[Option<usize>]) -> bool {
        let mut last: Option<usize> = None;
        for (j, p) in paired.iter().enumerate() {
            let Some(i) = p else { continue };
            if prev[*i] != next[j] || last.is_some_and(|l| *i <= l) {
                return false;
            }
            last = Some(*i);
        }
        true
    }

    proptest! {
        /// The optimality claim in the module doc: the pairing is a *longest*
        /// common subsequence, not merely an in-order matching (#298).
        #[test]
        fn the_pairing_is_a_longest_common_subsequence(
            prev in prop::collection::vec("[ab]{1,3}", 0..7),
            next in prop::collection::vec("[ab]{1,3}", 0..7),
        ) {
            let paired = align(&prev, &next);
            prop_assert_eq!(paired.len(), next.len());
            prop_assert!(is_valid_pairing(&prev, &next, &paired), "not a common subsequence");
            let carried = paired.iter().filter(|p| p.is_some()).count();
            prop_assert_eq!(carried, brute_force_lcs(&prev, &next), "not maximal");
        }

        /// A carry never aliases: two new lines never pair with one previous
        /// slot, so the anchors handed out stay unique (the invariant
        /// `resolve_anchor`'s first-occurrence rule relies on).
        #[test]
        fn a_previous_slot_pairs_with_at_most_one_new_line(
            prev in prop::collection::vec("[ab]{1,3}", 0..8),
            next in prop::collection::vec("[ab]{1,3}", 0..8),
        ) {
            let paired = align(&prev, &next);
            let mut seen = std::collections::HashSet::new();
            for p in paired.into_iter().flatten() {
                prop_assert!(seen.insert(p), "slot {p} paired twice");
            }
        }
    }

    #[test]
    fn an_unchanged_file_pairs_every_line_with_itself() {
        let lines: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|l| (*l).to_owned())
            .collect();
        assert_eq!(align(&lines, &lines), vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn a_fully_disjoint_file_pairs_nothing() {
        let prev: Vec<String> = ["one", "two"].iter().map(|l| (*l).to_owned()).collect();
        let next: Vec<String> = ["six", "seven"].iter().map(|l| (*l).to_owned()).collect();
        assert_eq!(align(&prev, &next), vec![None, None]);
    }

    #[test]
    fn an_empty_side_pairs_nothing() {
        let one: Vec<String> = ["one"].iter().map(|l| (*l).to_owned()).collect();
        assert_eq!(align(&[], &one), vec![None]);
        assert!(align(&one, &[]).is_empty());
    }

    #[test]
    fn repeated_checksums_pair_with_distinct_slots() {
        // A per-checksum lookup would pair both copies with slot 0 and mint an
        // anchor the file already holds.
        let prev = vec!["s1".to_owned(), "s1".to_owned()];
        let next = vec!["s1".to_owned(), "s1".to_owned()];
        assert_eq!(align(&prev, &next), vec![Some(0), Some(1)]);
    }

    #[test]
    fn a_greedy_first_match_would_be_shorter_here() {
        // prev: x is needed *after* y for the longest pairing; a cursor walk
        // that takes the first x burns the pairing on the wrong x.
        let prev = vec!["x".to_owned(), "y".to_owned(), "x".to_owned()];
        let next = vec!["y".to_owned(), "x".to_owned()];
        let paired = align(&prev, &next);
        assert_eq!(paired.iter().filter(|p| p.is_some()).count(), 2);
        assert_eq!(brute_force_lcs(&prev, &next), 2);
    }
}
