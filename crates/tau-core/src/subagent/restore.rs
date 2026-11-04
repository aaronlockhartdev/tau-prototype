//! The on-disk ground truth of a parent's sub-agent children (ticket #250):
//! the re-adopt and the fleet's disk half read the child session files —
//! the last `subagent` state entry and the child→parent header link —
//! which survive a restart where the supervisor's in-memory registry does
//! not. The snapshot's `subagents` projection over these records lives in
//! `snapshot.rs`.
//!
//! The records are read as [`SubagentRecord`] arms (spec §5.2): the shape
//! these files carry had no type, so every reader decoded it by hand and each
//! supplied its own defaults for fields the writer always fills.

use super::record::{ChildStateRecord, SpawnRecord, SubagentRecord};
use super::{ChildState, ContextMode};
use crate::session::{Entry, SessionStore, list_workspace};
use tau_protocol::EntryKind;

use std::borrow::Cow;
use std::path::Path;

/// The head window that carries a child's spawn record (#302): a fresh or
/// compacted child writes it as its first entry. A fork child's log is the
/// parent's copied tree, so its own record sits after every record the copy
/// carried — the tail window is searched first. Outside both windows there is
/// no provenance to read.
pub(crate) const SPAWN_HEAD: usize = 8;

/// The tail window that carries a child's last state record (#302): every
/// drive opens with a `running` record and closes with a park, stop, fail or
/// completion, so the records after the last state record are at most one
/// interrupted turn's. Outside the window the child reads as parked — the
/// #221 post-restart fallback — and re-adopt drops it.
pub(crate) const STATE_TAIL: usize = 256;

/// One of the parent's children as reconstructed from disk alone (the
/// re-adopt's input, #250): the child's name (the file header's title), the
/// minted handle (the parent's spawn records), the child session id, the
/// spawn record it carries (its durable provenance, ADR-0001), and its last
/// recorded lifecycle state. A child with no terminal state is not a
/// re-adopt candidate and is not returned.
#[derive(Debug, Clone)]
pub struct DiskChild {
    pub name: String,
    pub handle: String,
    pub child: String,
    /// The child's spawn record. A fork child's copied tree can carry the
    /// parent's older spawn records, so the newest one is its own; `None` is
    /// a file that carries none (hand-made, or a log from before the record).
    pub spawn: Option<SpawnRecord>,
    pub state: ChildState,
    pub created: u64,
}

impl DiskChild {
    /// The child's agent type and context mode. A file carrying no spawn
    /// record reads as the built-in spawn — the one place that default
    /// lives, where the readers each used to carry their own copy.
    #[must_use]
    pub fn provenance(&self) -> (String, ContextMode) {
        self.spawn.as_ref().map_or_else(
            || ("general".to_owned(), ContextMode::Fresh),
            |s| (s.agent_type.clone(), s.context_mode),
        )
    }
}

/// A `DiskChild`'s protocol projection (the snapshot's disk half, #250):
/// the name and the spawn-record handle are the display/identity the GUI
/// already carries for the child; the terminal state is its last recorded
/// ground truth.
#[must_use]
pub fn disk_child_to_info(d: &DiskChild) -> tau_protocol::SubagentInfo {
    let (agent_type, context_mode) = d.provenance();
    tau_protocol::SubagentInfo {
        handle: d.handle.clone(),
        child: d.child.clone(),
        agent_type,
        context_mode: match context_mode {
            ContextMode::Fresh => tau_protocol::ContextMode::Fresh,
            ContextMode::Compacted => tau_protocol::ContextMode::Compacted,
            ContextMode::Fork => tau_protocol::ContextMode::Fork,
        },
        state: d.state.clone().into(),
        last_message: None,
        usage: None,
        created: d.created,
        resume_contract: None,
    }
}

/// The child's last recorded lifecycle state, over entries already in
/// memory (issue #280): the fleet read opens each child file once and
/// re-uses the entries for both the state check and the spawn provenance.
/// The newest state record wins; a record that cannot state its detail
/// decodes to nothing, and the caller's rule (terminal-only for re-adopt,
/// parked for the snapshot) applies to the state the child did record.
pub(crate) fn last_state_from(entries: &[Entry]) -> Option<ChildState> {
    state_of(last_state_record(entries).as_ref())
}

/// The child's last lifecycle record, newest first. One scan for the two
/// readers of it — the re-adopt's terminal check and the snapshot's badge.
pub(crate) fn last_state_record(entries: &[Entry]) -> Option<ChildStateRecord> {
    newest(entries, |record| match record {
        SubagentRecord::State { state } => Some(state),
        _ => None,
    })
}

/// The child's own spawn record: the tail window newest-first, then the head
/// window newest-first. A fork child's log is the parent's copied tree, so a
/// record the copy carried is older than the child's own at the tail; a
/// reader that looks at the head first reads a sibling's provenance (#302
/// found the head-only case; this is the rest of it).
pub(crate) fn spawn_record(head: &[Entry], tail: &[Entry]) -> Option<SpawnRecord> {
    newest(tail, spawn_of).or_else(|| newest(head, spawn_of))
}

/// The spawn arm of a decoded record. The record's `parent` link names the
/// spawning session, which a record the copy carried also names: the
/// tail-first search is what separates the child's own spawn from a
/// sibling's.
fn spawn_of(record: SubagentRecord) -> Option<SpawnRecord> {
    match record {
        SubagentRecord::Spawn { record } => Some(record),
        _ => None,
    }
}

/// The newest entry that decodes to a sub-agent record of the wanted arm.
fn newest<T>(entries: &[Entry], arm: impl Fn(SubagentRecord) -> Option<T>) -> Option<T> {
    entries.iter().rev().find_map(|e| {
        if e.kind != EntryKind::Subagent {
            return None;
        }
        arm(SubagentRecord::from_payload(&e.payload)?)
    })
}

/// The recorded state as the live enum: the record owns the reason and the
/// stopper, and its `done` carries no output (the notify record owns that).
fn state_of(record: Option<&ChildStateRecord>) -> Option<ChildState> {
    Some(match record? {
        ChildStateRecord::Running => ChildState::Running,
        ChildStateRecord::Idle { waiting_on } => ChildState::Idle {
            waiting_on: *waiting_on,
        },
        ChildStateRecord::Awaiting => ChildState::Awaiting,
        ChildStateRecord::Done => ChildState::Done { output: None },
        ChildStateRecord::Failed { reason } => ChildState::Failed {
            reason: reason.clone(),
        },
        ChildStateRecord::Stopped { by } => ChildState::Stopped { by: *by },
    })
}

/// The parent's children from disk (the re-adopt's input, #250): every
/// on-disk session whose header names `parent` (and is not archived),
/// carrying its name, minted handle, spawn record, and last recorded
/// state — a child whose state is not terminal (or that has none) is
/// dropped: re-adopt is terminal-only (design §5).
pub fn disk_children(cwd: &Path, parent: &str, parent_entries: Option<&[Entry]>) -> Vec<DiskChild> {
    // The parent's spawn records map child session id → minted handle, so a
    // re-adopted child keeps the exact handle the GUI and the parent model
    // already address it by (not a synthesized session-id handle). The
    // open path threads the parent's already-loaded entries through (issue
    // #280); a caller without them (the `subagent_list` command, the
    // re-adopt) re-reads the parent's file.
    let mut handle_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // The threaded entries are borrowed (a `Cow::Borrowed`); a caller
    // without them re-reads the parent's file (a `Cow::Owned`).
    let spawn_records: Cow<'_, [Entry]> = if let Some(entries) = parent_entries {
        Cow::Borrowed(entries)
    } else {
        // The whole parent log by design (#302): every spawn record in the
        // parent's history names a child, and the store has no read narrower
        // than a position range (spec §3).
        let mut pstore = SessionStore::for_workspace(cwd, parent);
        Cow::Owned(
            pstore
                .open()
                .and_then(|()| pstore.entries_range(0, usize::MAX))
                .unwrap_or_default(),
        )
    };
    for e in spawn_records.iter() {
        if e.kind == EntryKind::Subagent
            && e.payload["event"] == "spawn"
            && let (Some(child), Some(handle)) = (
                e.payload.get("child").and_then(|v| v.as_str()),
                e.payload.get("handle").and_then(|v| v.as_str()),
            )
        {
            handle_of.insert(child.to_owned(), handle.to_owned());
        }
    }
    let mut out = Vec::new();
    for m in list_workspace(cwd) {
        if m.archived || m.parent.as_deref() != Some(parent) {
            continue;
        }
        // One open of the child file, two bounded reads: the spawn record
        // sits at the log's head (a fork child appends its own to the copied
        // tree), its last state record at the tail (#302 — the fleet read
        // never parses a child's whole file; #280's one open).
        let mut cstore = SessionStore::for_workspace(cwd, &m.id);
        if cstore.open().is_err() {
            continue;
        }
        let Ok(head) = cstore.entries_range(0, SPAWN_HEAD) else {
            continue;
        };
        let Ok(tail) = cstore.entries_tail(STATE_TAIL) else {
            continue;
        };
        let Some(state) = last_state_from(&tail) else {
            continue;
        };
        if !state.is_terminal() {
            continue;
        }
        let name = m.title.clone().unwrap_or_else(|| m.id.clone());
        let handle = handle_of
            .get(&m.id)
            .cloned()
            .unwrap_or_else(|| m.id.clone());
        out.push(DiskChild {
            name,
            handle,
            child: m.id,
            spawn: spawn_record(&head, &tail),
            state,
            created: m.created,
        });
    }
    out
}
