use super::*;

use crate::agent::testkit::*;

/// Deleting a queued message by its id removes that one item and keeps the
/// rest in order (ticket #223, id-addressed by #346).
#[tokio::test]
async fn remove_queued_deletes_by_id_and_keeps_order() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let agent = make_agent(dir.path(), provider);
    agent.send("one", Lane::FollowUp);
    agent.send("two", Lane::FollowUp);
    agent.send("three", Lane::Steering);
    let ids = agent
        .queued_items()
        .iter()
        .map(|i| i.id.clone())
        .collect::<Vec<_>>();
    // Deleting the middle item's id keeps both neighbors, in order.
    let removed = agent.remove_queued(&ids[1]).unwrap();
    assert_eq!(removed, "two");
    let items = agent.queued_items();
    assert_eq!(
        items.iter().map(|i| i.text.clone()).collect::<Vec<_>>(),
        vec!["one", "three"]
    );
    // The survivors keep their ids: a re-projection never renumbers a row.
    assert_eq!(
        items.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        vec![ids[0].clone(), ids[2].clone()]
    );
}

/// An id no queued item carries — out of bounds is unrepresentable once items
/// are addressed by id — removes nothing.
#[tokio::test]
async fn remove_queued_unknown_id_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let agent = make_agent(dir.path(), provider);
    agent.send("one", Lane::FollowUp);
    assert!(agent.remove_queued("q99").is_none());
    assert_eq!(agent.queued_items().len(), 1);
    let live = agent.queued_items()[0].id.clone();
    assert!(agent.remove_queued(&live).is_some());
    assert!(agent.queued_items().is_empty());
}

/// A force send takes the head of the queue and still carries its own id:
/// ids mint per item, never per position (#346).
#[tokio::test]
async fn a_force_send_jumps_the_queue_with_its_own_id() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let agent = make_agent(dir.path(), provider);
    agent.send("first", Lane::FollowUp);
    agent.send("jump", Lane::Force);
    let items = agent.queued_items();
    assert_eq!(
        items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
        vec!["jump", "first"]
    );
    assert_eq!(items[0].id, "q2", "each item carries the id it minted");
    assert_eq!(items[1].id, "q1");
}

/// A stop set while the queue has pending messages pre-empts the whole
/// drain: nothing is appended, and the queue is intact (ticket #223 — the
/// auto-send on stop). The next explicit send resumes the drain: a send
/// clears the stop and the queued items start their turns.
#[tokio::test]
async fn a_stop_preempts_the_drain_and_a_send_resumes_it() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        sse("two", &[]),
        sse("three", &[]),
    ]));
    let agent = make_agent(dir.path(), provider);
    agent.send("one", Lane::FollowUp);
    agent.send("two", Lane::FollowUp);
    agent.send("three", Lane::FollowUp);
    // The stop lands while the first turn is in flight (the GUI's stop
    // button): the in-flight call is cut and the drain must stop there.
    agent.stop();
    agent.process().await.unwrap();
    let entries = entries_of(&agent.inner.lock().store);
    assert!(entries.is_empty(), "no turn ran: {entries:?}");
    // The queue is intact for the next explicit send.
    let items = agent.queued_items();
    assert_eq!(
        items.iter().map(|i| i.text.clone()).collect::<Vec<_>>(),
        vec!["one", "two", "three"]
    );
    // The next explicit send clears the stop and resumes the drain.
    agent.send("go", Lane::FollowUp);
    agent.process().await.unwrap();
    let entries = entries_of(&agent.inner.lock().store);
    let users: Vec<&str> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::User)
        .map(|e| e.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(users, vec!["one", "two", "three", "go"]);
}

/// The #148 lifecycle pinned at the agent: a notified steering report mints
/// its entry id at queue time and fires the wire upsert then (the GUI shows
/// the report while the parent's turn is in flight), and the file line lands
/// at dequeue under that same id — one id, two emissions, one entry. Delete
/// the pre-mint and this goes red at the upsert: the card would appear only
/// when the line is written, which is the id-order property #148 exists for.
#[tokio::test]
async fn a_notified_steer_mints_at_queue_time_and_lands_under_that_id() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![sse("answer", &[])]));
    let agent = make_agent(dir.path(), provider);
    // The entry tee (ADR-0008): the wire-only upsert and the committed
    // append both ride it, so the test sees each emission's id.
    let fired = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let f = Arc::clone(&fired);
    agent.set_entry_event_hook(Some(Arc::new(move |entry: &Entry| {
        f.lock().push(entry.id.clone());
    })));
    agent.send_notified_steer("the child reports", "child-1".to_owned());
    // Queue time: the upsert fired, the file line did not.
    assert_eq!(fired.lock().len(), 1, "the queue-time upsert did not fire");
    let minted = fired.lock()[0].clone();
    assert!(
        entries_of(&agent.inner.lock().store).is_empty(),
        "a queue-time file line lands ahead of the in-flight assistant (#148)"
    );
    agent.send("go", Lane::FollowUp);
    agent.process().await.unwrap();
    let entries = entries_of(&agent.inner.lock().store);
    let report = entries
        .iter()
        .find(|e| e.payload["lane"] == serde_json::json!("steering"))
        .expect("the steering report's entry is missing");
    // The dequeue wrote the line under the id the mint reserved: the id the
    // wire saw, and the file holds exactly one copy of it.
    assert_eq!(report.id, minted, "the line did not land under the id");
    assert_eq!(report.payload["source"], serde_json::json!("child-1"));
    assert_eq!(
        entries.iter().filter(|e| e.id == minted).count(),
        1,
        "the pre-mint issued a second entry"
    );
    // Wire emissions: the queue-time upsert and the committed append carry
    // the one id, twice and no more (the follow-up's user entry and the
    // turn's assistant entry ride the same tee with their own ids).
    assert_eq!(
        fired.lock().iter().filter(|id| **id == minted).count(),
        2,
        "the pre-mint's id reached the wire more than upsert + commit: {fired:?}"
    );
    assert_eq!(fired.lock()[0], minted, "the upsert is the first emission");
}

/// The mint-failure path of the same lifecycle: a store whose file cannot be
/// read has no id to give the report, so the item is queued with none — no
/// wire card, since an id the file may already hold is the collision 3B's
/// mint refuses — and the drain surfaces the storage error instead of
/// appending.
#[tokio::test]
async fn a_notified_steer_with_no_mintable_id_queues_without_a_wire_card() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![sse("answer", &[])]));
    let agent = make_agent(dir.path(), provider);
    let fired = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let f = Arc::clone(&fired);
    agent.set_entry_event_hook(Some(Arc::new(move |entry: &Entry| {
        f.lock().push(entry.id.clone());
    })));
    // The file grows past the mirror's length with a line that is not an
    // entry: the mint's refresh re-reads it, and the read's failure is the
    // mint's failure (3B: a mint is a Result).
    {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(agent.inner.lock().store.path())
            .unwrap()
            .write_all(b"not an entry line\n")
            .unwrap();
    }
    agent.send_notified_steer("the child reports", "child-1".to_owned());
    assert!(
        fired.lock().is_empty(),
        "an id the store cannot build fired a wire card: {fired:?}"
    );
    assert!(agent.has_pending(), "the report was dropped, not queued");
    // The drain surfaces the storage error: never a silent success, never a
    // mint at the file's first id (the 80 GB collision's shape).
    let err = agent.process().await.unwrap_err();
    assert!(
        matches!(err, AgentError::Session(_)),
        "the mint failure did not surface: {err}"
    );
    assert!(
        fired.lock().is_empty(),
        "a colliding entry reached the wire"
    );
}
