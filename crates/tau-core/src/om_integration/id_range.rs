//! The entry-id number domain (finding 5C): the one place this seam turns a
//! session entry id into a number, so every ordering site compares numbers.

use super::BufferedChunk;

/// The id's place in the session's chronology, when it has one. Ids are minted
/// `{:08}` from a `u64` counter (`SessionStore::mint_id`), so its number is the
/// order; the padded string follows that order only while every id is 8 wide,
/// and inverts at the pad's ceiling (`'99999999' > '100000000'`).
#[must_use]
pub(crate) fn id_number(id: &str) -> Option<u64> {
    id.parse::<u64>().ok()
}

/// The closed `[first, last]` entry-id span a buffered chunk's range names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdRange {
    first: u64,
    last: u64,
}

impl IdRange {
    /// The span a durable range tuple names. A range with an unnumbered end
    /// names no span: `None`, not a span floored at 0 — a range this build
    /// cannot place on the branch is no evidence about the branch.
    pub(crate) fn of(range: &(String, String)) -> Option<Self> {
        Some(Self {
            first: id_number(&range.0)?,
            last: id_number(&range.1)?,
        })
    }

    /// The span ends at or behind `seen`: its raw is already observed, so the
    /// chunk is spent (#395).
    #[must_use]
    pub(crate) const fn ends_at_or_behind(self, seen: u64) -> bool {
        self.last <= seen
    }

    /// Whether the two closed spans share an entry.
    #[must_use]
    pub(crate) fn overlaps(self, other: Self) -> bool {
        self.first.max(other.first) <= self.last.min(other.last)
    }
}

/// Two chunk ranges intersect: the double-observe guard a plain equality miss
/// leaves open — a late commit, a fork inheriting the parent's vec (#402).
/// Where both spans carry numbers, the numbers decide. A range with no span is
/// no evidence that its raw is observed, so it intersects nothing — equality
/// of unorderable ids is not coverage, and an unorderable range suppresses no
/// commit (production ranges come from `ObservedSpan`'s entry ids and always
/// carry numbers; the unorderable shape is a test's fixture id).
#[must_use]
pub(crate) fn ranges_overlap(a: &(String, String), b: &(String, String)) -> bool {
    match (IdRange::of(a), IdRange::of(b)) {
        (Some(a), Some(b)) => a.overlaps(b),
        _ => false,
    }
}

/// The end id of the chunk that reaches furthest along the branch: chunks
/// arrive in commit order, not id order (#402), and the order is the id's
/// number, not its digits. A chunk whose range names no span reaches
/// nowhere — and an unorderable cursor takes the whole branch anyway
/// (`branch_after`), so dropping it from the max keeps that guarantee.
#[must_use]
pub(crate) fn last_chunk_end(chunks: &[BufferedChunk]) -> Option<String> {
    chunks
        .iter()
        .filter_map(|c| Some((IdRange::of(&c.range)?.last, c.range.1.clone())))
        .max_by_key(|(end, _)| *end)
        .map(|(_, end)| end)
}
