//! The sub-agent entry's records, typed (spec §5.2, ADR-0001). Every
//! transition a child makes is a `subagent` entry in a session file, and
//! `EntryKind::Subagent` is the one kind with no payload type in
//! `tau_protocol::payload` — so the shape these writers emit was decoded by
//! hand at each reader: the spawn record read three times, the state record
//! twice, each with its own defaults for a field the writer always fills.
//!
//! The types are the core's, not the protocol crate's, on purpose: the wire
//! surface the GUI mirrors carries the fleet's *events* (`SubagentInfo`,
//! `SubagentState`), not the on-disk record, and the GUI's sub-agent card
//! reads the entry's free-form map. Typing the record is a storage concern
//! (ADR-0005: free-form at rest, typed in transit to the readers).
//!
//! The state arm rides `ChildStateRecord`, the mirror `snapshot.rs` has
//! always carried: the restart gives a `running` record a meaning of its own
//! — the process is gone, so the child reads parked — that a decoder of the
//! protocol's `SubagentState` cannot hold.
//!
//! Serde is byte-compatible with every payload the writers emit (the field
//! renames and the `event` tag are the keys the writers have always written),
//! verified by [`tests::every_written_record_round_trips`]. A record that
//! cannot state its detail is a decode `Err`, and the reader that asked for
//! it decides what a missing record means.

use super::states::{ChildState, ContextMode, StoppedBy, WaitingOn};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A child's spawn record: the agent type it was spawned as, its brief, the
/// context it was given, the parent link (the parent's session id and the
/// originating tool call id) that is the child's durable provenance
/// (ADR-0001), and the handle the parent minted for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRecord {
    #[serde(rename = "type")]
    pub agent_type: String,
    pub brief: String,
    pub context_mode: ContextMode,
    pub parent: String,
    pub call: String,
    /// The address the parent and the GUI use for this child, minted before
    /// the record is written (#1K): the child's own record carries it, so a
    /// restart recovers the address from the child's file alone.
    ///
    /// `Option` because the record has no version field: every file written
    /// before the field carries spawn records without it, and those keep
    /// decoding. A record with no handle reads as the address re-adopt has
    /// always produced for such a child — its session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

/// The child's record of a note to its parent (`parent_notify`): the text,
/// and the completion declaration — `done` with its object `output`, or a
/// rest that names its declared wait target (spec §5.2: the declaration is a
/// secondary hint, not the driver).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotifyRecord {
    pub text: String,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<WaitingOn>,
}

/// The persisted lifecycle record: a `ChildState` mirror that keeps the
/// `Done` output out of the file — the output lives in the notify record, so
/// writing it twice would put two copies of one fact on disk.
///
/// No `other` arm, unlike the protocol's `SubagentState`: the file was
/// written by this same build, so there is no newer name to keep — a record
/// that cannot state its detail decodes to nothing, and the reader declines
/// it rather than picking a state the core never wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChildStateRecord {
    Running,
    Idle {
        // The target is what `set_state` writes; a record that omits it
        // states no wait, and the restore declines it rather than picking a
        // target (the wire decoder's rule for a truncated frame, applied to
        // the record).
        waiting_on: WaitingOn,
    },
    Awaiting,
    Done,
    Failed {
        reason: String,
    },
    Stopped {
        by: StoppedBy,
    },
}

impl ChildStateRecord {
    /// The recorded state as the protocol's tagged enum (#346): the record
    /// carries the reason and the stopper, so the restart's badge says the
    /// same thing the live event did. Done's output is the notify record's,
    /// never the state entry's, so it reads back absent.
    #[must_use]
    pub fn to_protocol(&self) -> tau_protocol::SubagentState {
        match self {
            Self::Running => tau_protocol::SubagentState::Running,
            Self::Idle { waiting_on } => tau_protocol::SubagentState::Idle {
                waiting_on: (*waiting_on).into(),
            },
            Self::Awaiting => tau_protocol::SubagentState::Awaiting,
            Self::Done => tau_protocol::SubagentState::Done { output: None },
            Self::Failed { reason } => tau_protocol::SubagentState::Failed {
                reason: reason.clone(),
            },
            Self::Stopped { by } => tau_protocol::SubagentState::Stopped {
                by: (*by).into(),
                resume_contract: None,
            },
        }
    }

    /// The child's live state as its record: the discriminant plus the
    /// non-output variant fields only (the rule above).
    #[must_use]
    pub fn from_state(state: &ChildState) -> Self {
        match state {
            ChildState::Running => Self::Running,
            ChildState::Idle { waiting_on } => Self::Idle {
                waiting_on: *waiting_on,
            },
            ChildState::Awaiting => Self::Awaiting,
            ChildState::Done { .. } => Self::Done,
            ChildState::Failed { reason } => Self::Failed {
                reason: reason.clone(),
            },
            ChildState::Stopped { by } => Self::Stopped { by: *by },
        }
    }
}

/// One `subagent` entry, tagged on `event` — the key every writer has always
/// written. Internally tagged with flattened arms, so the JSONL line stays
/// the flat `{event, …}` record on disk while a reader matches an arm instead
/// of indexing keys by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SubagentRecord {
    /// The spawn: the child's provenance, written into the child's file.
    Spawn {
        #[serde(flatten)]
        record: SpawnRecord,
    },
    /// A lifecycle transition, written by the child's own state setter.
    State {
        #[serde(flatten)]
        state: ChildStateRecord,
    },
    /// A note the child sent its parent.
    Notify {
        #[serde(flatten)]
        note: NotifyRecord,
    },
}

impl SubagentRecord {
    /// A stored entry's payload as its record. `None` is the reader's own
    /// filter: the payload is not a record this build knows (a foreign
    /// `event` key, a record that cannot state its detail).
    #[must_use]
    pub fn from_payload(payload: &Value) -> Option<Self> {
        serde_json::from_value::<Self>(payload.clone()).ok()
    }

    /// The record as the JSONL line's payload.
    #[must_use]
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The writers' shapes, verbatim: every payload `spawn.rs`, `child.rs`
    /// and `notify.rs` emit today decodes to its arm and re-emits the same
    /// bytes. (The record's own key order is serde's; the store's canonical
    /// line sorts keys, so the file's bytes and its CRC are unchanged.)
    #[test]
    fn every_written_record_round_trips() {
        let records = [
            json!({"event":"spawn","type":"general","brief":"b","context_mode":"fresh","parent":"p","call":"c1"}),
            json!({"event":"state","state":"running"}),
            json!({"event":"state","state":"idle","waiting_on":"parent"}),
            json!({"event":"state","state":"awaiting"}),
            json!({"event":"state","state":"done"}),
            json!({"event":"state","state":"failed","reason":"boom"}),
            json!({"event":"state","state":"stopped","by":"user"}),
            json!({"event":"notify","text":"hi","done":false}),
            json!({"event":"notify","text":"done","done":true,"output":{"k":"v"}}),
            json!({"event":"notify","text":"rest","done":false,"waiting_on":"user"}),
        ];
        for stored in records {
            let record = SubagentRecord::from_payload(&stored).expect("a written record decodes");
            assert_eq!(
                serde_json::to_value(&record).expect("a record serializes"),
                stored,
                "the typed record is the stored line"
            );
        }
    }

    /// Negative: a record that cannot state its detail decodes to nothing, not
    /// to a default. These are the shapes the hand-parsers turned into
    /// `general`/`fresh` — a provenance the core never wrote.
    #[test]
    fn a_record_that_cannot_state_its_detail_decodes_to_nothing() {
        // The idle record's target is required.
        assert!(
            SubagentRecord::from_payload(&json!({"event": "state", "state": "idle"})).is_none()
        );
        // The spawn record's provenance fields are required.
        assert!(
            SubagentRecord::from_payload(
                &json!({"event":"spawn","brief":"b","context_mode":"fresh","parent":"p","call":"c1"})
            )
            .is_none()
        );
        // A context mode the core never writes is not silently a fresh spawn.
        assert!(
            SubagentRecord::from_payload(
                &json!({"event":"spawn","type":"general","brief":"b","context_mode":"sideways","parent":"p","call":"c1"})
            )
            .is_none()
        );
        // Not a sub-agent record at all.
        assert!(
            SubagentRecord::from_payload(&json!({"event": "om", "active_observations": "x"}))
                .is_none()
        );
    }

    /// The `Done` record carries no output: the notify record owns it, so the
    /// file keeps one copy of the fact.
    #[test]
    fn the_done_record_carries_no_output() {
        let record = ChildStateRecord::from_state(&ChildState::Done {
            output: Some(json!({"k": "v"})),
        });
        assert_eq!(record, ChildStateRecord::Done);
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            json!({"state": "done"}),
            "the output stays off the state record"
        );
    }

    /// The persisted state projects to the protocol's tagged state with its
    /// reason and stopper intact — the restart's badge says what the live
    /// event said.
    #[test]
    fn the_record_projects_to_the_protocol_state() {
        assert_eq!(
            ChildStateRecord::Failed {
                reason: "boom".into()
            }
            .to_protocol(),
            tau_protocol::SubagentState::Failed {
                reason: "boom".into()
            }
        );
        assert_eq!(
            ChildStateRecord::Idle {
                waiting_on: WaitingOn::Subagent,
            }
            .to_protocol(),
            tau_protocol::SubagentState::Idle {
                waiting_on: tau_protocol::WaitingOn::Subagent,
            }
        );
    }
}
