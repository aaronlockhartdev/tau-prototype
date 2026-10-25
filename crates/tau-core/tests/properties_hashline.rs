//! Anchor-stability properties across the tool boundary (#298): a read, an
//! edit, and a re-read must not re-label lines that survived, and the carry
//! the edit path computes must be a longest common subsequence of the two
//! files' line checksums — the same claim the primitive makes in isolation.
#![allow(clippy::unwrap_used, clippy::panic)]

use proptest::prelude::*;
use std::collections::{HashMap, HashSet};
use tau_core::hashline::line_checksum;
use tau_core::hashline::line_endings::Ending;
use tau_core::hashline::line_hashes;
use tau_core::hashline::plan_edits;
use tau_core::hashline::registry::Registry;
use tau_core::hashline::{Edit, PlanOptions};
use tau_core::hashline::{apply_edit, assemble};

/// The `(line, anchor)` pairs a file's current allocation shows the model.
fn shown(content: &str, hashes: &[String]) -> Vec<(String, String)> {
    content
        .split('\n')
        .map(str::to_owned)
        .zip(hashes.iter().cloned())
        .collect()
}

fn sums(content: &str) -> Vec<String> {
    content.split('\n').map(line_checksum).collect()
}

/// The LCS length over the checksum sequences, by DP — a test oracle only.
/// The production path is `hashline::alignment` (#300 keeps this as its
/// reference implementation for exactly this assertion).
fn lcs_len(prev: &[String], next: &[String]) -> usize {
    let mut row = vec![0usize; next.len() + 1];
    for p in prev {
        let mut diag = 0usize;
        for (j, n) in next.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if p == n {
                (diag + 1).max(row[j])
            } else {
                up.max(row[j])
            };
            diag = up;
        }
    }
    row[next.len()]
}

/// New lines that kept an anchor the file already held for a line with the
/// same checksum. Anchors are minted away from every held slot, so a carried
/// anchor is never a coincidence.
fn carried(before: &[(String, String)], after: &[(String, String)]) -> usize {
    let held: HashMap<String, String> = before
        .iter()
        .map(|(line, anchor)| (line_checksum(line), anchor.clone()))
        .collect();
    after
        .iter()
        .filter(|(line, anchor)| held.get(&line_checksum(line)).is_some_and(|a| a == anchor))
        .count()
}

/// New lines that did **not** keep such an anchor — the re-label count.
fn relabelled(before: &[(String, String)], after: &[(String, String)]) -> usize {
    after.len() - carried(before, after)
}

/// Lines the edit changed: same-position differences plus the length
/// difference (the insertions and deletions the pairing has to absorb).
fn changed_lines(before: &[String], after: &[String]) -> usize {
    let shared = before.len().min(after.len());
    let diff = (0..shared).filter(|k| before[*k] != after[*k]).count();
    diff + (before.len() - shared) + (after.len() - shared)
}

proptest! {
    /// The §7.3 bar on the unified path: an edit re-labels no more lines than
    /// it changed, and the carry it kept is a *longest* common subsequence —
    /// the greedy walk satisfied neither (#298).
    #[test]
    fn an_edit_relabels_no_more_lines_than_it_changed(
        lines in prop::collection::vec("[a-z]{4}", 4..14),
        start in 0usize..14,
        span in 1usize..4,
        reuse in 0usize..4,
    ) {
        let unique: Vec<String> = lines
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let content = unique.join("\n");
        let before = line_hashes(&content).unwrap();
        let start = start.min(unique.len() - 1);
        let end = start.saturating_add(span).min(unique.len() - 1);
        // The replacement reuses surviving lines (a move) and adds a fresh
        // one, so the pairing has real churn to resolve.
        let mut replacement: Vec<String> = unique
            .iter()
            .skip(end + 1)
            .take(reuse)
            .cloned()
            .collect();
        replacement.push(format!("fresh{start}"));
        let edit = Edit {
            from: before[start].clone(),
            to: before[end].clone(),
            content: replacement.join("\n"),
        };
        let edited = apply_edit(&content, &edit).unwrap();
        let before_pairs = shown(&content, &before);
        let after_pairs = shown(&edited.content, &edited.hashes);

        prop_assert_eq!(
            carried(&before_pairs, &after_pairs),
            lcs_len(&sums(&content), &sums(&edited.content)),
            "the carry is not a longest common subsequence"
        );
        prop_assert!(
            relabelled(&before_pairs, &after_pairs) <= changed_lines(&sums(&content), &sums(&edited.content)),
            "the edit re-labelled {} lines but changed {}",
            relabelled(&before_pairs, &after_pairs),
            changed_lines(&sums(&content), &sums(&edited.content))
        );
    }

    /// The two paths agree at the tool boundary: after the edit commits, a
    /// fresh read re-allocates exactly the anchors the edit computed — no
    /// second re-labelling pass (#298's "one primitive" claim).
    #[test]
    fn a_reread_after_an_edit_shows_the_edit_anchors(
        lines in prop::collection::vec("[a-z]{4}", 4..14),
        at in 0usize..14,
    ) {
        let unique: Vec<String> = lines
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let content = unique.join("\n");
        let mut reg = Registry::new();
        let (first, _) = reg.allocate("f", &content).unwrap();
        let at = at.min(unique.len() - 1);
        let planned = plan_edits(
            &content,
            Ending::Lf,
            &[Edit {
                from: first[at].clone(),
                to: first[at].clone(),
                content: format!("CHANGED{at}"),
            }],
            &PlanOptions {
                path: "f".into(),
                served: Some(
                    shown(&content, &first)
                        .into_iter()
                        .map(|(l, a)| (a, line_checksum(&l)))
                        .collect(),
                ),
                hashes: Some(first.clone()),
                ..PlanOptions::default()
            },
        )
        .unwrap();
        let new_content = assemble(&planned);
        reg.refresh_owned("f", &new_content, &planned.hashes);
        let (again, _) = reg.allocate("f", &new_content).unwrap();
        prop_assert_eq!(&again, &planned.hashes, "the re-read re-labelled what the edit anchored");
    }
}

#[test]
fn an_unchanged_reread_shows_the_same_anchors() {
    let content = "one\ntwo\nthree\n";
    let mut reg = Registry::new();
    let (a, _) = reg.allocate("f", content).unwrap();
    let (b, _) = reg.allocate("f", content).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_reread_of_a_shifted_file_keeps_every_survivor() {
    // An insertion at the top: every original line survives and the maximal
    // pairing carries all of them; the greedy walk re-minted all four.
    let before = "one\ntwo\nthree\n";
    let after = "zero\none\ntwo\nthree\n";
    let mut reg = Registry::new();
    let (a, _) = reg.allocate("f", before).unwrap();
    let (b, _) = reg.allocate("f", after).unwrap();
    assert_eq!(&b[1..], &a[..], "no original line lost its anchor");
}
