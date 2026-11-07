use super::*;

use crate::agent::Lane;
use crate::session::SessionStore;
use crate::subagent::testkit::*;
use serde_json::json;
use std::time::Duration;

/// The child's `subagent` state records, in file order: the record the
/// module's docs name as the ground truth.
fn state_records(dir: &std::path::Path, session: &str) -> Vec<String> {
    let mut store = SessionStore::for_workspace(dir, session);
    store.open().unwrap();
    store
        .entries_range(0, usize::MAX)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EntryKind::Subagent && e.payload["event"] == json!("state"))
        .map(|e| e.payload["state"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A child's session file, removed: the store refuses every append to a file
/// that is gone — it never resurrects a headerless one (ADR-0005) — so each
/// state record fails at the append.
fn child_file_gone(dir: &std::path::Path, session: &str) {
    std::fs::remove_file(SessionStore::for_workspace(dir, session).path()).unwrap();
}

/// The child a harness spawned, by handle.
fn child_of(sup: &Arc<Supervisor>, handle: &str) -> Arc<Child> {
    sup.children
        .lock()
        .values()
        .find(|c| c.handle == handle)
        .cloned()
        .expect("the spawn registers its child")
}

/// A child at its observed rest: `idle`, with that record in its file (the
/// pair's shared starting point). A resting drive stays alive and asleep
/// (review N8's wake), so the state read is the observation point, never the
/// drive's join.
fn resting_child(dir: &std::path::Path) -> (Arc<Supervisor>, Arc<Child>, String) {
    let (sup, _bridge, _factory, _parent) = harness_full(
        dir,
        vec![sse("spawning", &[])],
        vec![vec![sse("", &[])]],
        vec![Duration::ZERO],
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
    let child = child_of(&sup, &s.handle);
    (sup, child, s.session_id)
}

/// A child whose every turn pauses mid-stream (the factory's delay): the
/// window a test removes its file in, so the turn ends against a file that
/// refuses appends.
fn slow_child(
    dir: &std::path::Path,
    turns: Vec<String>,
) -> (Arc<Supervisor>, Spawned, Vec<Duration>) {
    let delays = vec![Duration::from_millis(300)];
    let (sup, _bridge, _factory, _parent) = harness_full(
        dir,
        vec![sse("spawning", &[])],
        vec![turns],
        delays.clone(),
        SubAgents::default(),
    );
    let s = sup
        .spawn("general", "work", Some(ContextMode::Fresh), "c0")
        .unwrap();
    (sup, s, delays)
}

/// The positive: a record that lands advances the live view and is in the
/// file — the view and the record agree.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_state_record_that_appends_advances_the_view_and_lands_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (_sup, child, session) = resting_child(dir.path());

    child.set_state(&ChildState::Running).unwrap();

    assert!(matches!(child.state(), ChildState::Running));
    assert_eq!(
        state_records(dir.path(), &session),
        vec!["idle".to_owned(), "running".to_owned()],
        "the rest wrote `idle`; the transition is the record after it"
    );
}

/// The negative (audit #3G): the append is the commit point, so a refused
/// record leaves the view at the state a record did make. The live
/// `subagent_state`/snapshot read is the view, so a view that ran ahead of
/// the file is a state the child's record never made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_state_record_that_fails_to_append_leaves_the_prior_state() {
    let dir = tempfile::tempdir().unwrap();
    let (_sup, child, session) = resting_child(dir.path());
    child_file_gone(dir.path(), &session);

    let err = child
        .set_state(&ChildState::Running)
        .expect_err("the append to a gone file is refused");
    assert!(
        err.contains("state entry failed"),
        "the Err names the entry that failed: {err}"
    );
    assert!(
        matches!(child.state(), ChildState::Idle { .. }),
        "a state the record never made is not the child's state"
    );
}

/// A resting child's stop writes no record (spec §5.2: it already rests in
/// the recorded state), so it is the no-op ack whatever the file can do.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_of_a_resting_child_is_the_ack_whether_the_file_can_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, child, session) = resting_child(dir.path());
    child_file_gone(dir.path(), &session);

    let msg = sup.stop(&child.handle, StoppedBy::User).unwrap();
    assert!(
        msg.contains("resting"),
        "a resting child's stop is the no-op ack: {msg}"
    );
    assert!(matches!(child.state(), ChildState::Idle { .. }));
}

/// The command sites' policy (audit #3G): a transition whose record the file
/// refuses fails the command — the tool's caller is the party that can act on
/// storage being down — and the state it names is not the child's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_whose_state_record_is_refused_fails_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, s, _) = slow_child(
        dir.path(),
        vec![sse("working", &[]), sse("still working", &[])],
    );
    // Mid-stream: the child is Running, so the stop's transition is a record.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        sup.state_info(&s.handle).unwrap().state,
        ChildState::Running
    ));
    child_file_gone(dir.path(), &s.session_id);

    let err = sup
        .stop(&s.handle, StoppedBy::User)
        .expect_err("a stop whose record is refused is not an ack");
    assert!(
        err.contains("state entry failed"),
        "the stop's Err is the entry failure: {err}"
    );
    assert!(
        !SessionStore::for_workspace(dir.path(), &s.session_id)
            .path()
            .exists(),
        "the refused record is the stop's only durable effect, and it is not on disk"
    );
    s.drive.await.unwrap();
    // The drive ends on the stop's wake (a child left asleep holds a
    // concurrency slot, #250); the durable record never said `stopped`.
    assert!(
        matches!(
            child_of(&sup, &s.handle).state(),
            ChildState::Stopped { .. }
        ),
        "the live view lands terminal, the record never did"
    );
}

/// A resume whose record the file refuses is a failed command: the child
/// stays in the state its record names (resumable there), and no drive is
/// started on a state the file does not carry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_whose_state_record_is_refused_fails_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, child, session) = resting_child(dir.path());
    let gen_before = child.drive_gen.load(Ordering::SeqCst);
    child_file_gone(dir.path(), &session);

    let err = sup
        .message(&child.handle, Some("continue".into()), Lane::Steering, None)
        .expect_err("a resume whose record is refused is not an ack");
    assert!(
        err.contains("state entry failed"),
        "the resume's Err is the entry failure: {err}"
    );
    assert!(
        matches!(child.state(), ChildState::Idle { .. }),
        "the resume the record never made did not start"
    );
    assert_eq!(
        child.drive_gen.load(Ordering::SeqCst),
        gen_before,
        "no drive was started on a state the file does not carry"
    );
}

/// The observed rest whose record the file refuses cannot park the child:
/// the view would stay `Running` with no record behind it, and the drive's
/// loop head would run the child again, forever. The rest is met as a
/// failure — terminal, view-moved, parent-woken (audit #3G).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rest_whose_state_record_is_refused_lands_failed_and_wakes_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, bridge, _factory, _parent) = harness_full(
        dir.path(),
        vec![sse("spawning", &[])],
        vec![vec![sse("one", &[]), sse("two", &[])]],
        vec![Duration::from_millis(300)],
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
    // The resumed turn is in flight when its file goes: the rest that follows
    // has nowhere to record the state it observed.
    sup.message(&s.handle, Some("continue".into()), Lane::Steering, None)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    child_file_gone(dir.path(), &s.session_id);

    s.drive.await.unwrap();
    assert!(
        matches!(
            sup.state_info(&s.handle).unwrap().state,
            ChildState::Failed { .. }
        ),
        "the rest that could not record its state is a failure, not a child stuck running"
    );
    assert!(
        bridge
            .wakes
            .lock()
            .iter()
            .any(|w| matches!(w.kind, WakeKind::Failed)),
        "a failed child always notifies the parent (ADR-0001 wake rules)"
    );
}
