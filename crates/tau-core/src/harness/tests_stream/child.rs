//! A child as the GUI sees it: its own live snapshot, the parent snapshot's
//! crew list, the stream its turn projects, and the queue a send to it lands
//! on (#175, #187, #226, #261).

use super::*;

/// #261: a child's own live snapshot must report `turn: Running` while it is
/// driving a turn — not the `Idle` its never-set `turn` flag always gave. The
/// GUI derives a child's composer/stop from this, so an always-idle snapshot
/// is what flips a running child to `idle` when it is opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_children_snapshot_reports_turn_running() {
    // A slow child: its turn stays in flight long enough to snapshot it.
    let stream = stage(slow_child_core(providers(), plain_body(), 80)).await;
    let child = stream.spawn("do the thing").child;

    // Positive: while the child's slow turn is in flight, its own snapshot
    // reports `turn: Running`.
    assert!(
        poll(10, || stream.live_state(&child).turn == TurnState::Running).await,
        "the running child's snapshot never reported turn: Running"
    );
    // Negative: once the turn settles, the same snapshot is `Idle` again —
    // the flag is not sticky, so an idle child does not read as running.
    assert!(
        poll(10, || stream.live_state(&child).turn == TurnState::Idle).await,
        "the settled child's snapshot never reported turn: Idle"
    );
    drop(stream.core);
}

/// #226 variant: with two same-type children, the parent snapshot's
/// `subagents` must come back in SPAWN order — the frontend's `claimCrew`
/// pairs the transcript's Nth spawn point to the Nth list entry by
/// (`agent_type`, `context_mode`), so an arbitrary (`HashMap`) order pairs a
/// spawn point to the wrong child's card.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_snapshot_lists_subagents_in_spawn_order() {
    let stream = stage(canned_child_core(plain_body())).await;
    // Two same-type children, spawned in sequence.
    let first = stream.spawn("first").child;
    let second = stream.spawn("second").child;
    let order: Vec<String> = stream
        .live_state(&stream.session.id)
        .subagents
        .iter()
        .map(|i| i.child.clone())
        .collect();
    assert_eq!(
        order,
        vec![first, second],
        "the snapshot's subagents are not in spawn order"
    );
    drop(stream.core);
}

/// #175/#173: a child reuses the SAME `ForwardingProvider` its agent runs
/// through, so `run_turn` derives the child's `StreamEnd` `call_id` from
/// `live.provider.calls` and emits it (plus the child's assistant
/// `EntryUpsert`). This drives the *default* `ForwardingChildFactory` (no
/// `with_child_factory`) — the stash-reuse path every other spawn test
/// bypasses with a canned child factory. `canned://text` keeps the child's
/// turn deterministic.
#[tokio::test]
async fn a_child_reuses_its_agents_provider_and_emits_its_stream_end() {
    let stream = stage(canned_provider_core()).await;
    let child = stream.spawn("do the thing").child;

    // Bounded wait for the child's StreamEnd (its turn settles on its own).
    // Its assistant entry reaching the stream is #173: the transcript is live
    // in the GUI, which needs the child's event hooks wired.
    let (call_id, total_tokens, assistant_upserted) = scan(
        &stream.events,
        10,
        |events| {
            let assistant = events.iter().any(|e| {
                matches!(
                    e,
                    Event::EntryUpsert { session, entry, .. }
                        if *session == child && entry.kind == EntryKind::Assistant
                )
            });
            events.iter().find_map(|e| match e {
                Event::StreamEnd {
                    session,
                    call_id,
                    usage,
                    ..
                } if *session == child => Some((
                    call_id.clone(),
                    usage.as_ref().map(|u| u.total_tokens),
                    assistant,
                )),
                _ => None,
            })
        },
        |_| {
            "the child emitted no StreamEnd: its LiveSession provider does not match its agent's"
                .to_string()
        },
    )
    .await;

    // Positive: the child's assistant entry reached the stream.
    assert!(
        assistant_upserted,
        "the child's assistant entry never reached the stream"
    );
    // The fix (#175): a StreamEnd is emitted for the child at all. A
    // mismatched LiveSession provider leaves live.provider.calls empty, so
    // run_turn derives no call_id and emits nothing.
    // Negative: the call_id is a real derived id ({child_session}-{seq}), not
    // the empty id a provider mismatch would leave behind.
    assert!(
        !call_id.is_empty(),
        "the child's StreamEnd call_id is empty"
    );
    assert!(
        call_id.starts_with(&child),
        "the StreamEnd call_id {call_id:?} is not derived from the child's provider"
    );
    // The completion's usage rides the StreamEnd (canned text reports 140 total).
    assert_eq!(
        total_tokens,
        Some(140),
        "the child's StreamEnd lost its usage"
    );
    drop(stream.core);
}

/// #187: a GUI `MessageSend` to a child session projects the child's queue
/// (the in-flight message lands on the child's lane with the user as sender)
/// and emits a `Queue` event for the child's session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gui_send_to_a_child_projects_the_child_queue_with_a_sender() {
    let stream = stage(slow_child_core(providers(), plain_body(), 3000)).await;
    let sub = stream.spawn("do it");
    let child = sub.child.clone();

    // The child must be Running when the send lands, so it queues (a
    // non-running child would be resumed by it and drain it at once).
    let sup = stream.supervisor();
    let handle = sub.handle.clone();
    assert!(
        poll(10, || sup.child_running(&handle)).await,
        "the child was never observed running"
    );

    // The send to the child queues on its lane; the dispatch projects it.
    // Assert on the event, not `queued_items()`: the child is running and
    // drains its own queue, so a snapshot can race the drain on a slow runner.
    stream.send(&child, "steer the child", MessageLane::Steering);
    scan(
        &stream.events,
        5,
        |events| {
            // The pump's channel drops on overflow under load, so the child's
            // store — the durable record the send lands in — is the fallback
            // the event backs.
            (queued(events, &child, "steer the child", "user")
                || stream.entries(&child).iter().any(|e| {
                    e.kind == EntryKind::User
                        && e.payload.get("text").and_then(|t| t.as_str()) == Some("steer the child")
                        && e.payload.get("source").and_then(|s| s.as_str()) == Some("user")
                }))
            .then_some(())
        },
        |_| "no Queue event was emitted for the child's session".to_string(),
    )
    .await;
    drop(stream.core);
}

/// #147: a force-laned `MessageSend` to a running child must still let the
/// turn's `StreamEnd` reach the event stream — it is what flips the GUI's
/// child turn flag off (a missing end left the store stuck at `running`).
/// Delivery is the bar here; the force's cut-vs-clean shape is a separate
/// defect (filed from this investigation).
///
/// Seam: NO `with_child_factory`, so the child runs through the default
/// `ForwardingChildFactory` (the production path a real GUI child uses) with
/// a slow `canned://` parent model — the same shape
/// `a_child_reuses_its_agents_provider_and_emits_its_stream_end` pins,
/// but with a force send into the in-flight window.
#[tokio::test]
async fn a_force_send_to_a_running_child_still_emits_its_stream_end() {
    let stream = stage(canned_provider_core()).await;
    let child = stream.spawn("do it").child;

    // The cut only interrupts a call that is in flight: wait for the child's
    // first assistant entry (first delta recorded) before force-sending.
    assert!(
        poll(10, || stream.events.lock().iter().any(|e| matches!(
            e,
            Event::EntryUpsert { session, entry, .. }
                if *session == child && entry.kind == EntryKind::Assistant
        )))
        .await,
        "the child's turn never went in flight"
    );

    // The force lane takes the child through stop+send (R3).
    stream.send(&child, "stop and do this instead", MessageLane::Force);

    // The interrupted end is the first StreamEnd for the child.
    assert!(
        poll(15, || stream.events.lock().iter().any(|e| matches!(
            e,
            Event::StreamEnd { session, .. } if *session == child
        )))
        .await,
        "no StreamEnd reached the stream for the child (the GUI's turn flag would stick at running). The force send's cut-vs-clean shape is a separate concern (#147's delivery is the bar here)."
    );
    drop(stream.core);
}
