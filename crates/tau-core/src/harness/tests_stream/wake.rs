//! The parent's wake: a child's rest reported to the parent that spawned it
//! (ADR-0001, spec §5.2). The wake's target is the leg's subject — where the
//! child sits when it reports decides where the GUI parks it.

use super::*;
use tau_protocol::SubagentState;

/// A spawned child's observed `done` wakes the parent: the Notified
/// parent's branch tagged with the child's session id.
#[tokio::test]
async fn a_spawned_child_finishes_and_wakes_the_parent() {
    // One scripted turn: parent_notify done with a structured output.
    let stream = stage(canned_child_core(done_body())).await;
    let info = stream.spawn("do the thing");
    // The observed lifecycle derives `done` from the child's plan file
    // (spec §5.2): seed a complete plan while the one-time pre-delay
    // holds the child's first call, so the declaration is confirmed.
    stream.seed_plan(&info.child, DONE_PLAN);
    // The child's scripted done-notify must wake the parent: the Notified
    // event reaches the stream, and its wake names where the child parked.
    let waiting_on = scan(
        &stream.events,
        5,
        |events| {
            events.iter().find_map(|e| match e {
                Event::SubagentEvent {
                    kind:
                        SubagentEventKind::Notified {
                            wake, waiting_on, ..
                        },
                    ..
                } if wake == "done" => Some(*waiting_on),
                _ => None,
            })
        },
        |_| "the parent was never woken by the child's done".to_string(),
    )
    .await;
    // Negative: a done wake parks the child nowhere, so it names no target.
    assert!(
        waiting_on.is_none(),
        "a done wake parks the child nowhere, so it names no target"
    );
    // The wake's message lands on the parent's branch tagged with the
    // child's session id.
    assert!(
        stream
            .entries(&stream.session.id)
            .iter()
            .any(|e| e.payload.get("source") == Some(&json!(info.child))),
        "no notification entry on the parent's branch"
    );
    // The child registered as an ordinary live session (the GUI can
    // open it like any session).
    assert!(
        stream.core.sessions.lock().get(&info.child).is_some(),
        "the child's live session was not registered"
    );
    drop(stream.core);
}

/// #368: a child whose rest ends on a question is parked on the human, and the
/// wake that reports the rest states that target. The GUI read used to infer it
/// and moved the child to its parent, which took the app's red case off the bell.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_parked_on_a_question_wakes_the_parent_parked_on_the_user() {
    let data = r#"data: {"type":"response.output_text.delta","delta":"QUESTION: which call should I patch?"}"#;
    let stream = stage(canned_child_core(format!("{data}\n\ndata: [DONE]\n\n"))).await;
    let child = stream.spawn("do the thing").child;
    let seen = scan(
        &stream.events,
        10,
        |events| {
            let woken = events.iter().any(|ev| {
                matches!(
                    ev,
                    Event::SubagentEvent {
                        kind: SubagentEventKind::Notified {
                            child: c,
                            wake,
                            waiting_on: Some(WaitingOn::User),
                            ..
                        },
                        ..
                    } if c == &child && wake == "waiting"
                )
            });
            woken.then(|| events.to_vec())
        },
        |events| format!("the question rest never woke the parent parked on the user: {events:?}"),
    )
    .await;
    // Positive: the state the rest recorded is the question itself — the wake
    // and the state say the same thing about where the child sits.
    assert!(
        seen.iter().any(|ev| matches!(
            ev,
            Event::SubagentEvent {
                kind: SubagentEventKind::State {
                    child: c,
                    state: SubagentState::Awaiting,
                    ..
                },
                ..
            } if c == &child
        )),
        "the question rest recorded no `awaiting` state for the child"
    );
    drop(stream.core);
}
