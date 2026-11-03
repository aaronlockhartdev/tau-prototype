use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::pump::MAX_BATCH;
use super::*;

/// The pump, with its sink recording one entry per batch it was handed.
fn spawn_pump(core: &Arc<Core>) -> (Arc<Mutex<Vec<usize>>>, JoinHandle<()>) {
    let batches = Arc::new(Mutex::new(Vec::<usize>::new()));
    let sink = Arc::clone(&batches);
    let task = tokio::spawn(pump::pump(Arc::clone(core), move |batch| {
        sink.lock().push(batch.len());
    }));
    (batches, task)
}

fn opened(cwd: String) -> Event {
    Event::System {
        workspace: "w".into(),
        session: None,
        kind: SystemEventKind::WorkspaceOpened {
            name: "w".into(),
            cwd,
        },
    }
}

/// Bounded wait for the pump to have shipped `want` batches; a frame window is
/// 16 ms, so the polls settle in a few of them.
async fn wait_batches(batches: &Arc<Mutex<Vec<usize>>>, want: usize) -> Vec<usize> {
    for _ in 0..500 {
        let got = batches.lock().clone();
        if got.len() >= want {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let have = batches.lock().len();
    panic!("the pump shipped {have} batches, wanted {want}");
}

#[tokio::test]
async fn a_burst_in_the_pipe_lands_as_one_batch() {
    let core = CoreBuilder::custom(BTreeMap::new()).build().unwrap();
    for i in 0..5 {
        core.emit(opened(format!("/w/{i}")));
    }
    let (batches, _task) = spawn_pump(&core);

    let got = wait_batches(&batches, 1).await;
    assert_eq!(got, vec![5], "the pipe's five events ride one frame");
    drop(core);
}

#[tokio::test]
async fn a_batch_stops_at_its_ceiling_and_the_rest_ships_on_the_next_call() {
    let core = CoreBuilder::custom(BTreeMap::new()).build().unwrap();
    let sent = MAX_BATCH + 8;
    for i in 0..sent {
        core.emit(opened(format!("/w/{i}")));
    }
    let (batches, _task) = spawn_pump(&core);

    let got = wait_batches(&batches, 2).await;
    assert_eq!(got[0], MAX_BATCH, "a sink call carries at most the ceiling");
    assert_eq!(got[1], sent - MAX_BATCH, "the remainder follows");
    assert_eq!(
        got.iter().sum::<usize>(),
        sent,
        "the ceiling bounds a batch, never the delivery"
    );
    drop(core);
}

#[tokio::test]
async fn a_frame_closes_so_a_later_event_is_its_own_batch() {
    let core = CoreBuilder::custom(BTreeMap::new()).build().unwrap();
    let (batches, _task) = spawn_pump(&core);

    core.emit(opened("/w/first".into()));
    let got = wait_batches(&batches, 1).await;
    assert_eq!(got, vec![1], "a lone event ships alone");

    for i in 0..3 {
        core.emit(opened(format!("/w/{i}")));
    }
    let got = wait_batches(&batches, 2).await;
    assert_eq!(got[1], 3, "events that arrive together share one frame");
    drop(core);
}

#[tokio::test]
async fn a_burst_reaches_the_sink_whole_and_in_order() {
    let core = CoreBuilder::custom(BTreeMap::new()).build().unwrap();
    for i in 0..200 {
        core.emit(opened(format!("/w/{i:03}")));
    }
    let seen = spawn_texts(&core);

    for _ in 0..500 {
        if seen.lock().len() >= 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let want: Vec<String> = (0..200).map(|i| format!("/w/{i:03}")).collect();
    assert_eq!(
        *seen.lock(),
        want,
        "batching ships every event, in sequence"
    );
    assert_eq!(
        core.pipe.dropped.load(Ordering::Relaxed),
        0,
        "a burst under the pipe's capacity drops nothing"
    );
    drop(core);
}

/// The pump whose sink records every delivered event's `cwd` in arrival order
/// — the sequence equality a batch count cannot show.
fn spawn_texts(core: &Arc<Core>) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    tokio::spawn(pump::pump(Arc::clone(core), move |batch| {
        for event in batch {
            let Event::System {
                kind: SystemEventKind::WorkspaceOpened { cwd, .. },
                ..
            } = event
            else {
                panic!("the pump delivered a foreign event: {event:?}");
            };
            sink.lock().push(cwd.clone());
        }
    }));
    seen
}
