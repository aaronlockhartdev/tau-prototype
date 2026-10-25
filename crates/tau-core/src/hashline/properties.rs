use super::*;

use proptest::prelude::*;
use std::collections::HashSet;

proptest! {
    /// Uniqueness is allocation, not the hash function: even when every
    /// line has the same canon, no two anchors may collide (spec §5.4).
    #[test]
    fn every_line_gets_a_unique_anchor(lines in prop::collection::vec("abc", 0..40)) {
        let content = lines.join("\n");
        let hashes = line_hashes(&content).unwrap();
        assert_eq!(hashes.len(), split_lines(&content).len());
        let mut seen = HashSet::new();
        for h in &hashes {
            assert!(seen.insert(h.clone()), "duplicate anchor {h}");
        }
    }

    /// The anchor is a function of canon + preceding allocation state:
    /// whitespace padding changes nothing the allocator sees, so the
    /// anchor set is identical (determinism, canon invariance).
    #[test]
    fn anchors_follow_the_canon_not_the_whitespace(lines in prop::collection::vec("abc", 1..20)) {
        let base: String = lines.join("\n");
        let padded: String = lines
            .iter()
            .map(|l| format!("  {l}\t"))
            .collect::<Vec<_>>()
            .join("\n");
        prop_assert_eq!(line_hashes(&base).unwrap(), line_hashes(&padded).unwrap());
    }

    /// Survivor reuse: an edit of one line leaves every other line's anchor
    /// in place (a single-line replacement shifts nothing). Distinct lines
    /// only: where lines repeat, several maximal pairings differ by which copy
    /// carries which anchor and `align` picks among them, so position identity
    /// is not the claim there (`repeated_checksums_pair_with_distinct_slots`
    /// covers the duplicate case at the pairing level).
    #[test]
    fn survivors_keep_their_anchor_across_an_edit(
        (count, at) in (5usize..15, 0usize..15),
    ) {
        let replacement = String::from("wxyz");
        let at = at.min(count - 1);
        let lines: Vec<String> = (0..count).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let hashes = line_hashes(&content).unwrap();
        let edit = Edit {
            from: hashes[at].clone(),
            to: hashes[at].clone(),
            content: replacement.clone(),
        };
        let edited = apply_edit(&content, &edit).unwrap();
        for (i, line) in lines.iter().enumerate() {
            if i == at {
                continue;
            }
            assert_eq!(edited.hashes[i], hashes[i], "line {i} ({line}) lost its anchor");
        }
    }

    /// A fresh 4-letter draw that misses the current set is stale: rejected,
    /// never fuzzy-matched.
    #[test]
    fn an_anchor_not_in_the_file_is_stale_not_fuzzy_matched(
        (lines, mut seed) in (prop::collection::vec("abc", 1..10), 0usize..HASH_SPACE),
    ) {
        let content = lines.join("\n");
        let hashes = line_hashes(&content).unwrap();
        let mut candidate = idx_to_hash(seed);
        while hashes.contains(&candidate) {
            seed = (seed + STRIDE) % HASH_SPACE;
            candidate = idx_to_hash(seed);
        }
        let edit = Edit {
            from: candidate.clone(),
            to: candidate,
            content: "x".into(),
        };
        assert!(matches!(
            apply_edit(&content, &edit),
            Err(EditError::Stale { .. })
        ));
    }

    /// A malformed anchor (wrong length or an out-of-alphabet char) can
    /// never resolve to an application.
    #[test]
    fn malformed_anchors_never_apply(
        (len, chars) in (0usize..5, prop::collection::vec("Z0~|@", 0..6)),
    ) {
        let mut anchor: String = chars.iter().take(len).cloned().collect();
        anchor.push('~');
        let edit = Edit {
            from: anchor.clone(),
            to: anchor,
            content: "x".into(),
        };
        assert!(apply_edit("one\ntwo\n", &edit).is_err());
    }

    /// render → apply_edit → content: the edited file is exactly the
    /// expected line splice, and it re-anchors to a fresh unique set.
    #[test]
    fn render_then_edit_reproduces_the_expected_content(
        (lines, a, b) in (prop::collection::vec("abc", 1..12), 0usize..12, 0usize..12),
    ) {
        let replacement = String::from("xyz");
        let content = lines.join("\n");
        let hashes = line_hashes(&content).unwrap();
        let from = a.min(lines.len() - 1);
        let to = b.min(lines.len() - 1).max(from);
        let edit = Edit {
            from: hashes[from].clone(),
            to: hashes[to].clone(),
            content: replacement.clone(),
        };
        let edited = apply_edit(&content, &edit).unwrap();
        let mut expected: Vec<String> = lines[..from].to_vec();
        expected.push(replacement.clone());
        expected.extend(lines[to + 1..].iter().cloned());
        assert_eq!(edited.content, expected.join("\n"));
        let re = line_hashes(&edited.content).unwrap();
        let mut seen = HashSet::new();
        for h in &re {
            assert!(seen.insert(h.clone()), "duplicate anchor {h}");
        }
    }

    /// #298's optimality claim, on generated inputs: the pairing the primitive
    /// hands out carries exactly as many survivors as the longest common
    /// subsequence — no fewer (it is maximal), no more (it is a common
    /// subsequence). Checked against two oracles built differently from each
    /// other and from the DP `align` runs, so a wrong pairing cannot validate
    /// itself.
    #[test]
    fn the_alignment_pairs_a_maximal_common_subsequence(
        (prev, next) in (
            prop::collection::vec("[a-c]{1,3}", 0..9),
            prop::collection::vec("[a-c]{1,3}", 0..9),
        ),
    ) {
        let prev: Vec<String> = prev.iter().map(|l| line_checksum(l)).collect();
        let next: Vec<String> = next.iter().map(|l| line_checksum(l)).collect();
        let paired = alignment::align(&prev, &next);

        let mut pairs = 0;
        let mut prev_idx: Option<usize> = None;
        for (j, p) in paired.iter().enumerate() {
            let Some(i) = p else { continue };
            pairs += 1;
            assert_eq!(prev[*i], next[j], "pair {j}<->{i} is not a checksum match");
            if let Some(pi) = prev_idx {
                assert!(*i > pi, "pairs are out of previous-file order at {j}");
            }
            prev_idx = Some(*i);
        }
        let by_selections = lcs_len_by_selection(&prev, &next);
        let by_subsets = alignment::brute_force_lcs(&prev, &next);
        assert_eq!(
            by_selections, by_subsets,
            "the two oracles disagree on {prev:?} -> {next:?}"
        );
        assert_eq!(
            pairs,
            by_selections,
            "{prev:?} -> {next:?}: the pairing is not maximal"
        );
    }

    /// The churn bar on the pure edit path: a one-line replacement re-labels
    /// only the line it changed — every other line keeps the anchor it had.
    #[test]
    fn a_line_replacement_relabels_only_the_line_it_changed(
        (count, at, replacement) in (2usize..30, 0usize..30, "[x-z]{1,4}"),
    ) {
        let at = at % count;
        let lines: Vec<String> = (0..count).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let before = line_hashes(&content).unwrap();
        let edited = apply_edit(
            &content,
            &Edit {
                from: before[at].clone(),
                to: before[at].clone(),
                content: replacement.clone(),
            },
        )
        .unwrap();
        let relabelled = edited
            .hashes
            .iter()
            .zip(before.iter())
            .filter(|(new, old)| new != old)
            .count();
        assert_eq!(
            relabelled,
            usize::from(replacement != lines[at]),
            "a one-line edit re-labelled {relabelled} lines"
        );
    }

    /// The same bar on the registry's re-read path (read → edit → read): an
    /// unchanged line keeps its anchor, so the model is never shown a fresh
    /// label for content it was already shown.
    #[test]
    fn a_registry_reread_relabels_no_survivor(
        (count, at, replacement) in (2usize..30, 0usize..30, "[x-z]{1,4}"),
    ) {
        let at = at % count;
        let lines: Vec<String> = (0..count).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let mut reg = registry::Registry::new();
        let (before, _) = reg.allocate("f", &content).expect("allocate");
        let mut edited_lines = lines.clone();
        edited_lines[at] = replacement;
        let (after, _) = reg
            .allocate("f", &edited_lines.join("\n"))
            .expect("allocate");
        for (i, (old, new)) in before.iter().zip(after.iter()).enumerate() {
            if i == at {
                continue;
            }
            assert_eq!(old, new, "line {i} lost its anchor on re-read");
        }
    }
}

/// #298's maximality claim on the shapes that broke the greedy walk, spelled
/// out: an insertion, a deletion, a mid-file replacement, a transposition, a
/// reorder, repeated checksums, and the empty / disjoint sides where nothing
/// may pair.
#[test]
fn the_shapes_the_greedy_walk_got_wrong_pair_maximally() {
    let cases: &[(&[&str], &[&str])] = &[
        (&["a", "b", "c"], &["a", "b", "c"]),
        (&["a", "b", "c"], &["x", "a", "b", "y", "c"]),
        (&["a", "b", "c", "d"], &["a", "c", "d"]),
        (&["a", "b", "c", "d"], &["a", "B", "c", "d"]),
        (&["a", "b", "c", "d"], &["d", "c", "b", "a"]),
        (&["a", "b", "c", "d"], &["b", "a", "c", "d"]),
        (&["x", "y", "x"], &["y", "x"]),
        (&["a", "b", "a", "c"], &["b", "a", "a", "c"]),
        (&["a", "a", "a"], &["a", "a"]),
        (&["a", "a"], &["a", "a", "a"]),
        (&["a", "b"], &["c", "d"]),
        (&[], &["a", "b"]),
        (&["a", "b"], &[]),
        (&[], &[]),
    ];
    for (prev, next) in cases {
        let prev: Vec<String> = prev.iter().map(|l| line_checksum(l)).collect();
        let next: Vec<String> = next.iter().map(|l| line_checksum(l)).collect();
        let carried = alignment::align(&prev, &next)
            .iter()
            .filter(|p| p.is_some())
            .count();
        assert_eq!(
            carried,
            lcs_len_by_selection(&prev, &next),
            "{prev:?} -> {next:?}: the pairing is not maximal"
        );
    }
}

/// The LCS length over the previous side's slots: for every selection of
/// `prev` positions, walk the selection through `next` in order and keep the
/// longest one that lands entirely. An in-order pairing pairs a subset of
/// `prev` with an identical subset of `next`, so the longest landable
/// selection is attainable and never longer than the true LCS — and this never
/// runs the DP `align` runs, so a wrong DP cannot validate itself (#298).
fn lcs_len_by_selection(prev: &[String], next: &[String]) -> usize {
    (0..(1usize << prev.len()))
        .filter(|mask| {
            let mut cursor = 0usize;
            (0..prev.len()).filter(|k| mask & (1 << k) != 0).all(|k| {
                match next[cursor..].iter().position(|n| *n == prev[k]) {
                    Some(off) => {
                        cursor += off + 1;
                        true
                    }
                    None => false,
                }
            })
        })
        .map(|mask| (0..prev.len()).filter(|k| mask & (1 << k) != 0).count())
        .max()
        .unwrap_or(0)
}
