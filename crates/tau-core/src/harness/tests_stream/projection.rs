//! A message sent into a live turn: the report a child's wake leaves on the
//! parent's file (#148), and the in-flight growth a tool call streams to its
//! card (#197). Both are order questions between the queue and the file.

use super::*;

/// #148: a child's done report landing while the parent's call is in flight
/// must not invert the parent's file. The report's entry is minted at queue
/// time (its upsert fires then, so the GUI shows the report now) but its
/// line is written at the turn loop's dequeue — after the in-flight
/// assistant line lands. File order stays equal to id order.
#[tokio::test]
async fn a_notified_steer_lands_in_id_order_while_the_parent_call_is_in_flight() {
    // The parent's first call hangs in prefill for 2 s (the in-flight
    // window); the child finishes in ~0.8 s, inside it.
    let stream = stage(slow_child_core(
        canned_slow_first_provider(),
        done_body(),
        400,
    ))
    .await;
    let sub = stream.spawn("do it");
    stream.send(&stream.session.id, "go", MessageLane::FollowUp);

    // The steer is drained mid-turn: wait for the turn to start, then settle.
    let running = || stream.turn_running(&stream.session.id);
    assert!(poll(10, running).await, "the parent's turn never started");
    assert!(
        poll(10, || !running()).await,
        "the parent's turn never settled"
    );

    // The parent's file: every entry line in strict id order.
    let entries = stream.entries(&stream.session.id);
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(
        ids, sorted,
        "file line order diverges from id order: {ids:?}"
    );
    // The report is present, attributed to the child.
    assert!(
        report_from(&entries, &sub.child),
        "the steering report is missing from the file: {entries:?}"
    );
    // Its upsert fired at queue time (the GUI shows the report while the
    // parent's turn is in flight).
    let child = sub.child.clone();
    scan(
        &stream.events,
        5,
        |events| {
            events
                .iter()
                .any(|e| {
                    matches!(
                        e,
                        Event::EntryUpsert { session, entry, .. }
                            if *session == stream.session.id
                                && entry.payload_value()["source"] == json!(child)
                    )
                })
                .then_some(())
        },
        |_| "no queue-time upsert for the steering report".to_string(),
    )
    .await;
    drop(stream.core);
}

/// Issue #197: a bash call streams its in-flight output to the entry upsert
/// hook before the final result lands — the card grows, then is replaced.
#[tokio::test]
async fn a_slow_bash_streams_inflight_output_before_the_final_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let core = CoreBuilder::custom(providers()).build().unwrap();
    let workspace = open_ws(&core, tmp.path()).await;
    // A bash command that produces output over ~300 ms: `echo hello` lands
    // immediately, then a sleep, then `echo world`.
    let body = canned_body_with_bash(r#"{\"command\":\"echo hello; sleep 0.3; echo world\"}"#);
    let live = manual_session(
        &core,
        &workspace,
        provider::canned(&body),
        TurnConfig::default(),
    );
    let collected = collect_events(&core);
    let session_id = live.meta.lock().id.clone();
    core.dispatch(Command::MessageSend {
        session: session_id.clone(),
        text: "run it".into(),
        lane: MessageLane::Steering,
    })
    .unwrap();

    // Wait for the turn to settle (StreamEnd).
    let events = scan(
        &collected,
        10,
        |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::StreamEnd { .. }))
                .then(|| events.to_vec())
        },
        |_| "no StreamEnd within 10 s".to_string(),
    )
    .await;

    // Collect all tool upserts for this session.
    let tools: Vec<&tau_protocol::snapshot::ViewEntry> = events
        .iter()
        .filter_map(|e| match e {
            Event::EntryUpsert { entry, .. } if entry.kind == EntryKind::Tool => Some(entry),
            _ => None,
        })
        .collect();
    assert!(!tools.is_empty(), "no tool upserts: {events:?}");
    let tool_id = tools[0].id.clone();

    // At least one in-flight upsert carries growing output ("hello") but not
    // the final result ("exit 0"): the card was visible mid-flight.
    let has_inflight = tools.iter().any(|t| {
        t.id == tool_id
            && t.payload_value()["output"] != serde_json::json!("")
            && !t.payload_value()["output"]
                .as_str()
                .is_some_and(|s| s.contains("exit 0"))
    });
    assert!(
        has_inflight,
        "no in-flight output upsert (the card never grew before completion): {tools:?}"
    );

    // The final upsert carries the full result.
    let has_final = tools.iter().any(|t| {
        t.id == tool_id
            && t.payload_value()["output"]
                .as_str()
                .is_some_and(|s| s.contains("exit 0"))
    });
    assert!(has_final, "no final result upsert: {tools:?}");
}
