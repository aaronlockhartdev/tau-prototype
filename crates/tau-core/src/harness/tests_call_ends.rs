use super::*;

use crate::harness::testkit::*;
use std::sync::atomic::AtomicUsize;

/// The turn's stream ends, in emission order: `(call_id, interrupted,
/// input_tokens, duration_secs)`.
type Ends = Vec<(String, bool, Option<u64>, Option<f64>)>;

/// The scripted turn's seam: one canned body per call, in order. The calls
/// it serves are the seam's own — `manual_session` wraps it in the
/// forwarding provider, so every call here lands in the session's ledger.
struct Scripted {
    bodies: Vec<String>,
    served: AtomicUsize,
}

impl Scripted {
    fn serving(bodies: Vec<String>) -> TurnProviderRef {
        Arc::new(Self {
            bodies,
            served: AtomicUsize::new(0),
        })
    }
}

impl TurnProvider for Scripted {
    fn call<'a>(
        &self,
        request: &ResponseRequest,
        sink: &'a mut dyn TurnSink,
    ) -> super::ProviderTurn<'a> {
        let n = self.served.fetch_add(1, Ordering::SeqCst);
        let body = self.bodies.get(n).cloned().unwrap_or_default();
        provider::canned(&body).call(request, sink)
    }
}

/// A turn torn down by a provider panic on its first call, answered by the
/// second. The panic lands mid-call, after the seam recorded its start.
struct PanicOnce {
    panicked: AtomicUsize,
}

impl TurnProvider for PanicOnce {
    fn call<'a>(
        &self,
        request: &ResponseRequest,
        sink: &'a mut dyn TurnSink,
    ) -> super::ProviderTurn<'a> {
        let first = self.panicked.fetch_add(1, Ordering::SeqCst) == 0;
        let body = if first {
            String::new()
        } else {
            text_answer("recovered", 30, 7)
        };
        let request = request.clone();
        Box::pin(async move {
            first.then(|| panic!("injected provider panic (call-end ledger)"));
            provider::canned(&body).call(&request, sink).await
        })
    }
}

fn frame(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

fn completed(input: u64, output: u64) -> String {
    frame(&format!(
        r#"{{"type":"response.completed","response":{{"usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
    )) + &frame("[DONE]")
}

/// A content-free completed response: the model answered nothing, so the
/// agent records no entry (no content is no entry) while the call itself
/// completed with usage.
fn empty_completed(input: u64, output: u64) -> String {
    completed(input, output)
}

/// A bash call with its usage: the call gets an entry, so the turn continues
/// into the next body.
fn bash_call(input: u64, output: u64) -> String {
    frame(
        r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"bash","arguments":"{\"command\":\"ls\"}"}}"#,
    ) + &completed(input, output)
}

/// A streamed answer with its usage.
fn text_answer(text: &str, input: u64, output: u64) -> String {
    frame(&format!(
        r#"{{"type":"response.output_text.delta","delta":"{text}"}}"#
    )) + &completed(input, output)
}

/// The same answer streamed as `parts` deltas: a stop mid-stream cuts it
/// between them, with room on both sides of the cut.
fn streamed_answer(parts: usize, input: u64, output: u64) -> String {
    (0..parts)
        .map(|i| {
            frame(&format!(
                r#"{{"type":"response.output_text.delta","delta":"w{i} "}}"#
            ))
        })
        .chain(std::iter::once(completed(input, output)))
        .collect()
}

fn ends_of(events: &[Event], session: &str) -> Ends {
    events
        .iter()
        .filter_map(|e| match e {
            Event::StreamEnd {
                session: s,
                call_id,
                interrupted,
                usage,
                duration_secs,
                ..
            } if s == session => Some((
                call_id.clone(),
                *interrupted,
                usage.as_ref().map(|u| u.input_tokens),
                *duration_secs,
            )),
            _ => None,
        })
        .collect()
}

/// Bounded wait for `want` stream ends for the session. The ends ride the
/// event pipe (the pump flushes on a frame), and a turn torn down by a panic
/// settles without one, so every caller bounds the wait and asserts on what
/// arrived.
async fn ends_after(collected: &Arc<Mutex<Vec<Event>>>, session: &str, want: usize) -> Ends {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ends = ends_of(&collected.lock(), session);
        if ends.len() >= want || tokio::time::Instant::now() >= deadline {
            return ends;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Bounded wait for a session's turn flag to clear: the settlement
/// guarantee, on every path including a task that dies in a panic.
async fn wait_settled(core: &Arc<Core>, session: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while core
        .sessions
        .lock()
        .get(session)
        .is_some_and(|l| l.turn.load(Ordering::SeqCst))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !core
            .sessions
            .lock()
            .get(session)
            .is_some_and(|l| l.turn.load(Ordering::SeqCst)),
        "the turn never settled"
    );
}

/// One session on `inner`, one send, driven to settlement: the turn's stream
/// ends. `stop_after` lands a user stop that many ms after the send.
async fn ends_of_one_turn(inner: TurnProviderRef, stop_after: Option<u64>) -> (String, Ends) {
    let core = CoreBuilder::custom(providers()).build().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let workspace = match core
        .dispatch(Command::WorkspaceOpen {
            cwd: tmp.path().to_string_lossy().into_owned(),
        })
        .unwrap()
    {
        CommandOutput::Workspace { workspace: w } => w,
        other => panic!("expected a workspace: {other:?}"),
    };
    let live = manual_session(&core, &workspace, inner, TurnConfig::default());
    let collected = collect_events(&core);
    let session_id = live.meta.lock().id.clone();
    core.dispatch(Command::MessageSend {
        session: session_id.clone(),
        text: "work".into(),
        lane: MessageLane::Steering,
    })
    .unwrap();
    if let Some(ms) = stop_after {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        core.dispatch(Command::MessageStop {
            session: session_id.clone(),
        })
        .unwrap();
    }
    wait_settled(&core, &session_id).await;
    let ends = ends_after(&collected, &session_id, 1).await;
    drop(core);
    (session_id, ends)
}

/// The reconciliation emits one end per call, and each carries the facts the
/// record carries: the entry-less completed call's end owes its own usage.
///
/// The turn: a bash call (entry, usage 111), then a content-free completed
/// call (usage 222, no entry). The entry-less call is the ledger's blind
/// spot — the positional join reads the record's facts for entries only, and
/// the entry-less pass ships `usage: None`.
#[tokio::test]
async fn an_entry_less_completed_call_ends_with_its_usage() {
    let (session_id, ends) = ends_of_one_turn(
        Scripted::serving(vec![bash_call(111, 1), empty_completed(222, 2)]),
        None,
    )
    .await;
    assert_eq!(
        ends,
        vec![
            (format!("{session_id}-00000001"), false, Some(111), None),
            (format!("{session_id}-00000002"), false, Some(222), None),
        ],
        "two calls, two ends; the entry-less completed call is not cut and carries its usage"
    );
}

/// A call aborted during a long prefill — no event ever reached the sink —
/// ends interrupted. The record has no usage to carry (the Completed frame
/// never arrived), so its end carries none.
#[tokio::test]
async fn a_call_aborted_in_prefill_ends_interrupted() {
    let (session_id, ends) = ends_of_one_turn(
        provider::prefill_hang(&text_answer("late", 10, 5), 60_000),
        Some(150),
    )
    .await;
    assert_eq!(
        ends,
        vec![(format!("{session_id}-00000001"), true, None, None)],
        "the prefill abort's end is interrupted, with no usage the record never got"
    );
}

/// The kill path: a stop that cuts a streaming call ends it interrupted,
/// once, with no usage the stream never reached.
#[tokio::test]
async fn a_cut_call_ends_interrupted_once() {
    // 10 deltas at 200 ms: the stop at 600 ms lands mid-stream, ~1.4 s of
    // margin on the cut.
    let (session_id, ends) = ends_of_one_turn(
        provider::canned_slow(&streamed_answer(10, 10, 5), 200),
        Some(600),
    )
    .await;
    assert_eq!(
        ends,
        vec![(format!("{session_id}-00000001"), true, None, None)],
        "the cut call: one interrupted end, no usage the stream never reached"
    );
}

/// A turn torn down by a provider panic leaves its started call in the
/// ledger; the next settlement closes it. Every call yields exactly one end.
#[tokio::test]
async fn a_panicked_turn_s_call_ends_at_the_next_settlement() {
    let core = CoreBuilder::custom(providers()).build().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let workspace = match core
        .dispatch(Command::WorkspaceOpen {
            cwd: tmp.path().to_string_lossy().into_owned(),
        })
        .unwrap()
    {
        CommandOutput::Workspace { workspace: w } => w,
        other => panic!("expected a workspace: {other:?}"),
    };
    let live = manual_session(
        &core,
        &workspace,
        Arc::new(PanicOnce {
            panicked: AtomicUsize::new(0),
        }),
        TurnConfig::default(),
    );
    let collected = collect_events(&core);
    let session_id = live.meta.lock().id.clone();
    core.dispatch(Command::MessageSend {
        session: session_id.clone(),
        text: "first".into(),
        lane: MessageLane::Steering,
    })
    .unwrap();
    // The task dies in the panic; the settle guard clears the flag, so the
    // first turn's call is settled and still owed its end.
    wait_settled(&core, &session_id).await;

    core.dispatch(Command::MessageSend {
        session: session_id.clone(),
        text: "second".into(),
        lane: MessageLane::Steering,
    })
    .unwrap();
    let ends = ends_after(&collected, &session_id, 2).await;
    assert_eq!(
        ends,
        vec![
            (format!("{session_id}-00000001"), true, None, None),
            (format!("{session_id}-00000002"), false, Some(30), None),
        ],
        "the panicked call ends interrupted at the next settlement; the recovery call ends clean with its usage"
    );
    drop(core);
}
