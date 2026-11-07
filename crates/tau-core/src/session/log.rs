//! The in-memory log read path (spec §3): a live session's paged reads are
//! served from the store's in-memory log — no file re-open and no per-line
//! CRC re-verify (the entries were verified at `open`, and only this store
//! appends to the file — one writer per session). Paging indexes match the
//! file-backed reads' exactly (empty lines keep their slots). A
//! not-yet-opened store falls back to the file-backed read.
//!
//! H10: the mirror is `Loaded`, and it carries the file length it was read
//! at (`at_len`). The store's state is `Option<Loaded>`, so there is no
//! `loaded` flag to fall out of step with the mirror, and no cached read can
//! reach the mirror's rows without the proof — `Loaded` is the only way to
//! name them.

use std::collections::HashSet;
use std::sync::Arc;

use super::{Entry, Error, SessionStore, fs};
use tau_protocol::snapshot::EntryMeta;

/// The mirror of a session file's entries, plus the length it is valid at.
///
/// The mirror is valid only at a known file length (H10): every row here was
/// read from, or written by, the bytes up to `at_len`. `refresh_if_grown`
/// re-opens when the file has grown past it, and an append moves `at_len`
/// with the bytes this store wrote — the length is never a second bookkeeping
/// line that has to be kept in step by hand.
///
/// The snapshot's skeleton (#316) is built here, at the two points the log
/// grows — `open` reads a whole file into a fresh mirror, the append path
/// adds the line this store wrote — so its rows cannot describe a file other
/// than the one the mirror was read from; a poll hands them out as they stand.
pub(super) struct Loaded {
    at_len: i64,
    ids: HashSet<String>,
    next: u64,
    /// The entry lines in file order (empty lines kept as `None` so the
    /// in-memory paging indexes match the file's).
    entries: Vec<Option<Entry>>,
    /// Each entry line's byte length (a snapshot's `size` field, served
    /// without re-serializing the entry).
    entry_len: Vec<usize>,
    /// One snapshot metadata row per entry, in the same order.
    rows: Arc<[EntryMeta]>,
    /// The log's newest assistant entry, whose usage a snapshot reports.
    last_assistant: Option<String>,
}

impl Loaded {
    /// An empty mirror proven at `at_len`, the file's length as read.
    pub(super) fn new(at_len: u64) -> Self {
        Self {
            at_len: i64::try_from(at_len).unwrap_or(i64::MAX),
            ids: HashSet::new(),
            next: 1,
            entries: Vec::new(),
            entry_len: Vec::new(),
            rows: Arc::from(Vec::new()),
            last_assistant: None,
        }
    }

    /// Issue the counter's next id and move it on, so two mints that land
    /// before any append reserve two ids.
    pub(super) fn take_next(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }

    pub(super) const fn at_len(&self) -> i64 {
        self.at_len
    }

    pub(super) fn count(&self) -> usize {
        self.entries.len()
    }

    /// The snapshot's rows and the log's newest assistant entry, as the mirror
    /// stands: a poll of an unchanged log is handed these with no copy.
    pub(super) fn skeleton(&self) -> (Arc<[EntryMeta]>, Option<String>) {
        (self.rows.clone(), self.last_assistant.clone())
    }
    pub(super) fn holds(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Newest first.
    pub(super) fn last(&self) -> Option<&Entry> {
        self.entries.iter().rev().flatten().next()
    }
    /// One entry line's row: its id feeds the mint's counter, its text feeds
    /// the id set the append guard reads, and it joins the snapshot's rows.
    pub(super) fn push_entry(&mut self, entry: Entry, line_len: usize) {
        // The parse default is 0, and 0 can only lower the counter —
        // never a collision: a minted id is `{:08}` of a u64 (mint_id), so
        // any file id textually equal to one parses and pushes the counter
        // past it. An id that fails to parse is a non-canonical line from
        // outside this build: it keeps its slot, and its number is invisible
        // to the mint.
        self.next = self.next.max(entry.id.parse::<u64>().unwrap_or(0) + 1);
        self.ids.insert(entry.id.clone());
        if entry.kind == tau_protocol::EntryKind::Assistant {
            self.last_assistant = Some(entry.id.clone());
        }
        // The rows ride snapshots as an `Arc`, so a growth is the one copy
        // that detaches from the shares (#316: an idle poll pays nothing).
        let size = u64::try_from(line_len).expect("entry line, well under u64::MAX");
        let mut rows: Vec<EntryMeta> = self.rows.to_vec();
        rows.push(crate::harness::entry_meta(&entry, size));
        self.rows = rows.into();
        self.entries.push(Some(entry));
        self.entry_len.push(line_len);
    }

    /// An empty line keeps its slot so the paging indexes match the file's.
    pub(super) fn push_empty(&mut self) {
        self.entries.push(None);
        self.entry_len.push(0);
    }

    /// Take one line this store appended into the mirror and move `at_len` to
    /// the file's new length: the line's bytes, plus what settling a torn
    /// tail changed. H10 replaces the post-append `stat` that re-synced a
    /// hand-kept length with the length this write is the proof of.
    pub(super) fn record_append(&mut self, entry: &Entry, line_len: usize, settle_delta: i64) {
        self.push_entry(entry.clone(), line_len);
        self.at_len +=
            settle_delta + i64::try_from(line_len).expect("entry line, well under i64::MAX") + 1;
    }

    /// Paged read by 0-based entry index, the mirror's rows with their line
    /// sizes.
    pub(super) fn entries_range(&self, start: usize, end: usize) -> Vec<(Entry, u64)> {
        let mut out = Vec::new();
        for (i, slot) in (0..self.entries.len()).zip(&self.entries) {
            let Some(entry) = slot else {
                continue;
            };
            if i >= end {
                break;
            }
            if i >= start {
                out.push((
                    entry.clone(),
                    u64::try_from(self.entry_len[i]).expect("entry offset, well under u64::MAX"),
                ));
            }
        }
        out
    }

    /// The entries after `cursor` (exclusive), oldest first.
    pub(super) fn entries_since(&self, cursor: &str) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut past = false;
        for slot in &self.entries {
            let Some(entry) = slot else {
                continue;
            };
            if past {
                out.push(entry.clone());
            } else if entry.id == cursor {
                past = true;
            }
        }
        out
    }

    /// File order, not branch order — the caveat ticket #302 names for a tail
    /// read applies to the mirror too.
    pub(super) fn entries_tail(&self, n: usize) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .entries
            .iter()
            .rev()
            .flatten()
            .take(n)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    pub(super) fn entry(&self, id: &str) -> Result<Entry, Error> {
        self.entries
            .iter()
            .flatten()
            .find(|e| e.id == id)
            .cloned()
            .ok_or_else(|| Error::Other(format!("no entry {id}")))
    }

    /// Visit every entry with its line size, borrowing each one.
    pub(super) fn visit(&self, mut visit: impl FnMut(&Entry, u64)) {
        for (i, slot) in self.entries.iter().enumerate() {
            if let Some(entry) = slot {
                visit(
                    entry,
                    u64::try_from(self.entry_len[i]).expect("entry offset, well under u64::MAX"),
                );
            }
        }
    }
}

impl SessionStore {
    /// Re-open when the file has grown past the length the in-memory log is
    /// current to (an external writer — the 10k fixture test appends via its
    /// own store): the log is refilled from the file, so a live read never
    /// serves a stale view. A current file is a `stat`. A store with no
    /// mirror opens, which is how it gets one.
    pub fn refresh_if_grown(&mut self) -> Result<(), Error> {
        let len = fs::metadata(self.path())
            .map(|m| m.len())
            .map_err(|e| Error::Other(e.to_string()))?;
        if self
            .log
            .as_ref()
            .is_none_or(|log| i64::try_from(len).unwrap_or(i64::MAX) > log.at_len())
        {
            self.open()?;
        }
        Ok(())
    }

    /// The dispatch the `&self` cached reads share (H10): the mirror where the
    /// store has one, the file-backed twin where it has never read the file.
    /// A mirror read is unreachable without the `&Loaded` proof, which only
    /// this match and the two in `visit_entries_cached`/`leaf_cached` hand out
    /// (those borrow `&mut self` and an `FnMut` visitor, so they cannot call
    /// through here).
    fn mirror<T>(
        &self,
        from_log: impl FnOnce(&Loaded) -> Result<T, Error>,
        from_file: impl FnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        match &self.log {
            Some(log) => from_log(log),
            None => from_file(),
        }
    }

    /// The file's id set as this store's mirror holds it. A store with no
    /// mirror holds nothing, so a caller that has not opened refuses.
    pub(super) fn file_holds(&self, id: &str) -> bool {
        self.log.as_ref().is_some_and(|log| log.holds(id))
    }

    /// Take a line this store appended into the mirror, moving the length it
    /// proves. `append_line` opens the mirror before it writes, so a store
    /// that appended has one (H10).
    pub(super) fn record_append(&mut self, entry: &Entry, line_len: usize, settle_delta: i64) {
        self.log
            .as_mut()
            .expect("append_line opens the log before it writes")
            .record_append(entry, line_len, settle_delta);
    }

    /// A paged read by 0-based entry index, served from the in-memory log
    /// when the store has one; the file-backed read otherwise.
    pub fn entries_range_cached(
        &self,
        start: usize,
        end: usize,
    ) -> Result<Vec<(Entry, u64)>, Error> {
        self.mirror(
            |log| Ok(log.entries_range(start, end)),
            || {
                self.entries_range(start, end)
                    .map(|v| v.into_iter().map(|e| (e, 0)).collect())
            },
        )
    }

    /// The in-memory twin of `entries_since` (the live session's paged
    /// reads; see `entries_range_cached`).
    pub fn entries_since_cached(&self, cursor: &str) -> Result<Vec<Entry>, Error> {
        self.mirror(
            |log| Ok(log.entries_since(cursor)),
            || self.entries_since(cursor),
        )
    }

    /// Visit the loaded log's entries in file order with their line sizes,
    /// borrowing each entry: a projection that reads an entry's identity and
    /// metadata pays no payload clone (the delete sweep's blob enumeration,
    /// `harness::Core::delete_session_files`).
    /// The index space and the not-yet-opened fallback match
    /// `entries_range_cached`.
    pub fn visit_entries_cached(&self, mut visit: impl FnMut(&Entry, u64)) -> Result<(), Error> {
        match &self.log {
            Some(log) => log.visit(&mut visit),
            None => {
                for entry in self.entries_range(0, usize::MAX)? {
                    visit(&entry, 0);
                }
            }
        }
        Ok(())
    }

    /// The in-memory twin of `entries_tail`, oldest first. The caveat ticket
    /// #302 names for a tail read applies here too: it is file order, not
    /// branch order, so it is safe only where the record it seeks is the
    /// file's tail, not a branch's.
    pub fn entries_tail_cached(&self, n: usize) -> Result<Vec<Entry>, Error> {
        self.mirror(|log| Ok(log.entries_tail(n)), || self.entries_tail(n))
    }

    /// The loaded log's slot count, the same index space as
    /// `entries_range_cached`: a bounded read starts after it (the snapshot
    /// skeleton's cursor, #316). A not-yet-opened store reports 0, so its
    /// caller reads from the file's start.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.log.as_ref().map_or(0, Loaded::count)
    }

    /// The store's skeleton read (#316, #1F): the mirror's rows where the
    /// store has one, the file projected in order where it has never read it
    /// (the file-backed read's line sizes are unknown, so they read 0).
    pub fn skeleton(&self) -> Result<(Arc<[EntryMeta]>, Option<String>), Error> {
        if let Some(log) = &self.log {
            return Ok(log.skeleton());
        }
        let mut rows: Vec<EntryMeta> = Vec::new();
        let mut last_assistant = None;
        for entry in self.entries_range(0, usize::MAX)? {
            if entry.kind == tau_protocol::EntryKind::Assistant {
                last_assistant = Some(entry.id.clone());
            }
            rows.push(crate::harness::entry_meta(&entry, 0));
        }
        Ok((rows.into(), last_assistant))
    }

    /// The in-memory twin of `entry` (the live session's read path).
    pub fn entry_cached(&self, id: &str) -> Result<Entry, Error> {
        self.mirror(|log| log.entry(id), || self.entry(id))
    }

    /// The in-memory twin of `leaf` (the live session's read path): the
    /// persisted header choice, else the last entry line in the log.
    pub fn leaf_cached(&mut self) -> Result<Option<Entry>, Error> {
        let Some(log) = &self.log else {
            return self.leaf();
        };
        if let Some(l) = &self.leaf {
            return log.entry(l).map(Some);
        }
        Ok(log.last().cloned())
    }

    /// Adopt a branch choice another store just persisted (fork/branch
    /// rewrite the file header with their own store): the live session's
    /// store takes the new leaf in memory, without a second header rewrite.
    pub fn adopt_leaf(&mut self, leaf_id: &str) {
        self.leaf = Some(leaf_id.to_owned());
    }
}
