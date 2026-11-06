use super::*;

use crate::harness::testkit::*;

/// Audit §6A: a `run_turn` delta read that swallows a storage failure into
/// an empty delta loses data on the record-derived wire (spec §3) — the
/// turn's `StreamEnd`, tool upserts and compactions vanish in silence, and
/// the session shows running forever (#261/#82). Both tests inject a fault
/// that outlives the agent's write path and lands on the settlement's read
/// of the session log, and both assert the failure is *named*: pre-fix,
/// `run_turn` ends with no `System{Error}` at all.
fn session_path(cwd: &std::path::Path, session: &str) -> std::path::PathBuf {
    cwd.join(".tau")
        .join("sessions")
        .join(format!("{session}.jsonl"))
}

/// The turn settles: the flag clears — the `StreamEnd` arrived, or the named
/// failure took the turn home (the `TurnSettleGuard`, #267).
async fn wait_settled(core: &Arc<Core>, session_id: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while core
        .sessions
        .lock()
        .get(session_id)
        .is_some_and(|l| l.turn.load(Ordering::SeqCst))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn send(core: &Arc<Core>, session_id: &str, text: &str) {
    core.dispatch(Command::MessageSend {
        session: session_id.to_owned(),
        text: text.to_owned(),
        lane: MessageLane::Steering,
    })
    .unwrap();
}

fn named_failure(
    collected: &Arc<Mutex<Vec<Event>>>,
    session_id: &str,
    which: &str,
) -> Option<String> {
    collected.lock().iter().find_map(|e| match e {
        Event::System {
            session: Some(s),
            kind: SystemEventKind::Error { message },
            ..
        } if *s == session_id && message.contains(&format!("the {which} read")) => {
            Some(message.clone())
        }
        _ => None,
    })
}

fn count_streams(collected: &Arc<Mutex<Vec<Event>>>, session_id: &str, end: bool) -> usize {
    collected
        .lock()
        .iter()
        .filter(|e| {
            if end {
                matches!(e, Event::StreamEnd { session, .. } if *session == session_id)
            } else {
                matches!(e, Event::StreamStart { session, .. } if *session == session_id)
            }
        })
        .count()
}

/// The delta read's fault, injected mid-turn: the record file is deleted
/// while the provider streams. The turn's settlement appends refuse (their
/// own named failures) and the delta read has no file to read — the audit's
/// symptom is `StreamStart` with no `StreamEnd` and no error, which is what
/// the swallow produces.
#[tokio::test]
async fn a_turn_whose_record_vanishes_mid_flight_names_the_delta_failure() {
    let core = CoreBuilder::custom(providers()).build().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let workspace = open_ws(&core, cwd.path()).await;
    let live = manual_session(
        &core,
        &workspace,
        provider::canned_slow(&canned_body(), 20),
        TurnConfig::default(),
    );
    let session_id = live.meta.lock().id.clone();
    let collected = collect_events(&core);
    // One settled turn: the session's record and cursor exist.
    send(&core, &session_id, "first");
    wait_settled(&core, &session_id).await;

    // The faulted turn: the file goes once the call has started (its
    // `StreamStart` is the event whose end the swallow erases) and before
    // the settlement's writes and reads.
    send(&core, &session_id, "faulted");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while count_streams(&collected, &session_id, false) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the faulted turn's stream never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    std::fs::remove_file(session_path(cwd.path(), &session_id)).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(message) = named_failure(&collected, &session_id, "delta") {
            assert!(
                message.contains("turn record unreadable"),
                "the failure names the loss: {message}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no delta-failure error surfaced: {:?}",
            collected.lock().clone()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The events the record implies are not derived from a delta that never
    // arrived: only the settled turn's end exists.
    assert_eq!(
        count_streams(&collected, &session_id, true),
        1,
        "the faulted turn derived a `StreamEnd` from an unreadable record"
    );
    // The turn is settled: the named failure took the turn home, so the
    // session is not wedged running (#261).
    assert!(!live.turn.load(Ordering::SeqCst));
    drop(core);
}

/// The cursor's fault: the header's `leaf` names an entry line the file
/// does not hold — the shape a truncation or a foreign rewrite leaves. The
/// store's `open` parses the header and verifies every entry CRC, and the
/// cursor read has no entry to start from. Pre-fix, `leaf()`'s `.ok()`
/// swallowed that into "this is the session's first turn" and the whole log
/// answered as the delta; now the leaf read's failure is named.
#[tokio::test]
async fn a_header_leaf_the_log_does_not_hold_names_the_cursor_failure() {
    let core = CoreBuilder::custom(providers()).build().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let workspace = open_ws(&core, cwd.path()).await;
    let live = manual_session(
        &core,
        &workspace,
        provider::canned_slow(&canned_body(), 20),
        TurnConfig::default(),
    );
    let session_id = live.meta.lock().id.clone();
    let collected = collect_events(&core);
    // One settled turn leaves a header leaf pointing at a real entry.
    send(&core, &session_id, "first");
    wait_settled(&core, &session_id).await;

    // The fault: rewrite the header line so its leaf names an entry the
    // file has never held. The entry lines and their CRCs are untouched.
    let path = session_path(cwd.path(), &session_id);
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = raw.lines().map(str::to_owned).collect();
    let mut header: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&lines[0]).unwrap();
    header.insert(
        "leaf".into(),
        serde_json::Value::String("deadbeefdeadbeef".into()),
    );
    lines[0] = serde_json::to_string(&header).unwrap();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    // A store opens the poisoned file fresh: the header parses, the entries
    // verify, and the cursor the header names is no entry the log holds.
    let mut store = SessionStore::for_workspace(cwd.path(), &session_id);
    store.open().unwrap();
    let err = store
        .leaf_cached()
        .expect_err("the header leaf is no entry the log holds");
    assert!(
        err.to_string().contains("no entry deadbeefdeadbeef"),
        "the cursor miss is named: {err}"
    );

    // The live session sends into the poisoned file: the turn's cursor read
    // fails, and the failure is named on the GUI's error path.
    send(&core, &session_id, "faulted");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(message) = named_failure(&collected, &session_id, "leaf") {
            assert!(
                message.contains("turn record unreadable"),
                "the failure names the loss: {message}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no cursor-failure error surfaced: {:?}",
            collected.lock().clone()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The send was not run and is not lost: it stays in the agent's queue,
    // the turn is settled, and no stream of the faulted send exists.
    let items = live.agent.queued_items();
    assert!(
        items.iter().any(|i| i.text == "faulted"),
        "the accepted message must stay queued: {items:?}"
    );
    assert_eq!(count_streams(&collected, &session_id, false), 1);
    assert_eq!(count_streams(&collected, &session_id, true), 1);
    wait_settled(&core, &session_id).await;
    assert!(!live.turn.load(Ordering::SeqCst));
    drop(core);
}
