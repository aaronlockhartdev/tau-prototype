//! The child's resting place, as the `subagent_state` tool states it (audit
//! 6J): the state carries the target, and no second field mirrors it. The
//! protocol's `SubagentInfo` lost `waiting_on` in #346 (the state's own arm is
//! on the wire); the core's carried a derived copy with an invented
//! `Awaiting → user` rule, read by one formatter.

use super::testkit::*;
use super::*;
use crate::tools;
use serde_json::json;

/// The state names where it parks: an `idle` carries its declared target, the
/// `awaiting` state implies the human (the rest that ends on a question,
/// #368), and a state that parks nowhere names none.
#[test]
fn a_state_names_where_it_parks() {
    assert_eq!(
        ChildState::Idle {
            waiting_on: WaitingOn::Subagent,
        }
        .waiting_on(),
        Some(WaitingOn::Subagent),
        "the idle state's own target"
    );
    assert_eq!(
        ChildState::Awaiting.waiting_on(),
        Some(WaitingOn::User),
        "a rest that ends on a question is parked on the human"
    );
    assert_eq!(ChildState::Running.waiting_on(), None);
    assert_eq!(
        ChildState::Done { output: None }.waiting_on(),
        None,
        "a finished child is parked nowhere"
    );
    assert_eq!(
        ChildState::Stopped {
            by: StoppedBy::Parent,
        }
        .waiting_on(),
        None
    );
}

/// The tool's line states the target the resting state carries, read from the
/// state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_state_tool_states_the_target_the_child_carries() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, _bridge, _parent) = harness(
        dir.path(),
        vec![sse("spawning", &[])],
        vec![vec![sse(
            "",
            &[notify_call("n1", "parked", false, None, Some("user"))],
        )]],
        SubAgents::default(),
    );
    let s = sup
        .spawn("general", "work", Some(ContextMode::Fresh), "c0")
        .unwrap();
    wait_for(|| {
        matches!(
            sup.state_info(&s.handle).unwrap().state,
            ChildState::Idle { .. }
        )
    });
    let out = route_parent(
        &sup,
        &tools::ToolCall {
            id: "c1".into(),
            name: "subagent_state".into(),
            args: json!({ "name": s.name }),
        },
    )
    .unwrap();
    assert!(
        out.contains("(waiting on user)"),
        "the declared target rides the line: {out}"
    );
}

/// The negative: a child at work, and one finished, park nowhere — their lines
/// state no target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_that_parks_nowhere_states_no_target() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, _bridge, _parent) = harness(
        dir.path(),
        vec![sse("spawning", &[])],
        vec![vec![sse("working", &[])]],
        SubAgents::default(),
    );
    let s = sup
        .spawn("general", "work", Some(ContextMode::Fresh), "c0")
        .unwrap();
    let line = |sup: &Arc<Supervisor>| {
        route_parent(
            sup,
            &tools::ToolCall {
                id: "c1".into(),
                name: "subagent_state".into(),
                args: json!({ "name": s.name }),
            },
        )
        .unwrap()
    };
    // Running: the drive is mid-turn, nothing is parked.
    let out = line(&sup);
    assert!(out.contains("running"), "{out}");
    assert!(!out.contains("waiting on"), "{out}");
    // Stopped: the terminal child is parked nowhere.
    sup.stop(&s.handle, StoppedBy::Parent).unwrap();
    wait_for(|| {
        matches!(
            sup.state_info(&s.handle).unwrap().state,
            ChildState::Stopped { .. }
        )
    });
    let out = line(&sup);
    assert!(out.contains("stopped"), "{out}");
    assert!(
        !out.contains("waiting on"),
        "a terminal child parks nowhere: {out}"
    );
}
