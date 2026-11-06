//! 2B/2C: one stop flag per child session. The forwarding seam is what cuts a
//! stream (`ForwardSink::event` refuses a delta, `stop_signal` tears the
//! prefill request down) and it notes a call `completed` for the post-turn
//! reconciliation. Every one of those reads the seam's flag, so a seam handed
//! a flag no production code writes cannot cut anything.
//!
//! Every test drives the default `ForwardingChildFactory` (no
//! `with_child_factory`) — the production route a real child takes, the one
//! the canned-factory suites bypass — with the endpoint addressed at a local
//! mock that withholds its response, so a stop lands inside the prefill window
//! where only `stop_signal` can act.

use super::*;
use crate::harness::{Core, CoreBuilder, pump::pump};
use crate::session::Entry;
use std::time::Instant;
use tau_protocol::{Command, CommandOutput, ContextMode, Event};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{Duration, timeout};

/// The endpoint's silence: long enough that a stop lands inside prefill, short
/// enough that the uncut path settles inside the test's own window.
const SILENCE: Duration = Duration::from_secs(3);

/// One-shot endpoint: accepts `requests` connections, reads each request,
/// plays `body` after the silence, closes. The silence after the request
/// arrives is the prefill window — no byte has reached the sink in it.
async fn silent_endpoint(body: String, requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..requests {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            tokio::time::sleep(SILENCE).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n")
                .await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// A turn whose only content is one delta: a stop inside the silence must end
/// the call before that delta is ever read.
fn late_body() -> String {
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"late\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1,\"total_tokens\":11}}}\n\ndata: [DONE]\n\n".to_owned()
}

/// The core's event stream, collected.
fn collect_events(core: &Arc<Core>) -> Arc<Mutex<Vec<Event>>> {
    let collected = Arc::new(Mutex::new(Vec::<Event>::new()));
    let sink = Arc::clone(&collected);
    tokio::spawn(pump(Arc::clone(core), move |batch| {
        sink.lock().extend(batch.iter().cloned());
    }));
    collected
}

/// A root session whose provider is addressed at `base`.
fn core_over(base: &str) -> (Arc<Core>, String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let core = CoreBuilder::custom(
        [(
            "dev".to_owned(),
            crate::config::Provider::with_model(base, "model"),
        )]
        .into_iter()
        .collect(),
    )
    .build()
    .unwrap();
    let workspace = match core
        .dispatch(Command::WorkspaceOpen {
            cwd: tmp.path().to_string_lossy().into_owned(),
        })
        .unwrap()
    {
        CommandOutput::Workspace { workspace } => workspace,
        other => panic!("expected a workspace: {other:?}"),
    };
    let parent = match core
        .dispatch(Command::SessionNew {
            workspace: workspace.id.clone(),
            title: None,
            base_prompt: None,
        })
        .unwrap()
    {
        CommandOutput::Session { session } => session.id,
        other => panic!("expected a session: {other:?}"),
    };
    (core, parent, tmp)
}

/// The `general` child's spawn, as (handle, child session id).
fn spawn_child(core: &Arc<Core>, parent: &str) -> (String, String) {
    match core
        .dispatch(Command::SubagentSpawn {
            session: parent.into(),
            agent_type: "general".into(),
            brief: "work".into(),
            context_mode: ContextMode::Fresh,
        })
        .unwrap()
    {
        CommandOutput::Subagent { subagent } => (subagent.handle, subagent.child),
        other => panic!("expected a subagent: {other:?}"),
    }
}

/// The child's provider call, as the seam's own `StreamStart`: the request is
/// on the wire and the endpoint is silent, so a stop lands inside prefill.
async fn wait_for_call(collected: &Arc<Mutex<Vec<Event>>>, child: &str) {
    let seen = timeout(Duration::from_secs(10), async {
        loop {
            if collected
                .lock()
                .iter()
                .any(|e| matches!(e, Event::StreamStart { session, .. } if *session == child))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(seen.is_ok(), "the child's provider call never started");
}

/// The child's settled call, as its `StreamEnd`'s `interrupted`, within the
/// settlement's own deadline — the cut's, not the endpoint's. `None` means no
/// `StreamEnd` reached the stream inside the window.
async fn wait_for_end(
    collected: &Arc<Mutex<Vec<Event>>>,
    child: &str,
    within: Duration,
) -> Option<bool> {
    timeout(within, async {
        loop {
            if let Some(end) = collected.lock().iter().find_map(|e| match e {
                Event::StreamEnd {
                    session,
                    interrupted,
                    ..
                } if *session == child => Some(*interrupted),
                _ => None,
            }) {
                return end;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .ok()
}

/// The child's assistant entries, from its session file.
fn assistant_entries(tmp: &tempfile::TempDir, child: &str) -> Vec<Entry> {
    let mut store = SessionStore::for_workspace(tmp.path(), child);
    store.open().unwrap();
    store
        .entries_range(0, usize::MAX)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EntryKind::Assistant)
        .collect()
}

/// 2B: the child's forwarding seam and its agent stop on one flag.
/// `subagent_stop` writes the agent's flag (`Supervisor::stop`); the seam is
/// the transport's cut, so a seam handed a second flag never sees the stop.
/// 2B: the child, on the seam's flag identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_childs_forwarding_seam_stops_on_its_agents_flag() {
    let base = silent_endpoint(late_body(), 1).await;
    let (core, parent, _tmp) = core_over(&base);
    let (_, child) = spawn_child(&core, &parent);
    let live = core
        .live(&child)
        .expect("the child registers as a live session");
    assert!(
        Arc::ptr_eq(&live.provider.stop, &live.agent.stop_flag()),
        "the child's provider stop flag is not its agent's: the flag the stop \
         writes is invisible to the seam that cuts the stream"
    );
    drop(core);
}

/// 2B: a stop inside the prefill window tears the child's request down —
/// `ForwardSink::stop_signal` is the seam's half of that, and it polls the
/// seam's flag. On a seam watching a flag nobody writes, the poll never ends:
/// the request rides out the endpoint's silence and the child settles at the
/// provider's pace, not the user's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_stopped_in_prefill_is_cut_at_once() {
    let base = silent_endpoint(late_body(), 1).await;
    let (core, parent, tmp) = core_over(&base);
    let collected = collect_events(&core);
    let (handle, child) = spawn_child(&core, &parent);
    wait_for_call(&collected, &child).await;

    let stopped = Instant::now();
    core.dispatch(Command::SubagentStop { handle })
        .expect("the stop command");

    let end = wait_for_end(&collected, &child, Duration::from_secs(2)).await;
    assert!(
        end.is_some(),
        "a stopped child's turn settled only when the endpoint answered \
         ({} s in): its seam's stop_signal never saw the stop",
        stopped.elapsed().as_secs_f32()
    );
    assert!(
        stopped.elapsed() < Duration::from_secs(2),
        "the stop cut the turn {} s after it was asked for; the prefill \
         teardown polls the flag on a 50 ms cadence",
        stopped.elapsed().as_secs_f32()
    );
    assert_eq!(
        end,
        Some(true),
        "a stopped child's entry-less call read a clean completion"
    );
    // The record: the provider's late answer never reaches the child's file.
    let late = assistant_entries(&tmp, &child)
        .iter()
        .any(|e| e.payload["text"] == serde_json::json!("late"));
    assert!(
        !late,
        "the stopped child recorded the provider's late answer"
    );
    drop(core);
}

/// The negative on the same seam: with no stop, the silence is not a cut. The
/// child rides it to the answer and records it clean, so the cut above is the
/// stop's doing and not the fixture's shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_left_running_records_its_answer() {
    let base = silent_endpoint(late_body(), 1).await;
    let (core, parent, tmp) = core_over(&base);
    let collected = collect_events(&core);
    let (_handle, child) = spawn_child(&core, &parent);
    wait_for_call(&collected, &child).await;

    let end = wait_for_end(&collected, &child, Duration::from_secs(15)).await;
    assert_eq!(
        end,
        Some(false),
        "an unstopped child's call did not settle clean"
    );
    let entries = assistant_entries(&tmp, &child);
    assert!(
        entries.iter().any(|e| {
            e.payload["text"] == serde_json::json!("late")
                && e.payload["interrupted"] == serde_json::json!(false)
        }),
        "an unstopped child never recorded its provider's answer: {entries:?}"
    );
    drop(core);
}
