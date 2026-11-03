//! The event pump: the core's event channel → the sink (Tauri `emit` in the
//! app, a collector in tests). The pump is the transport's coalescer: one
//! sink call per 16 ms frame window, ceiling `MAX_BATCH` (#425) — AGENTS.md's
//! "coalesced `emit` batches … never a raw per-item loop". *N* concurrent
//! streams cost one IPC per frame, not *N*, and the bounded pipe (its slack
//! is measured in seconds of stream) keeps slack for a display that stalls.
//! The turn loop owns the frame cadence of a single stream (ADR-0008); the
//! ceiling is what keeps a resumed pipe's 1024 events out of one eval.

use std::time::Duration;

use tokio::sync::mpsc::{self, error::TryRecvError};

use super::{Arc, Core, Event};

/// The coalescing window: the display frame the turn loop already streams at
/// (ADR-0008), so a batched delivery costs at most the one frame the ADR
/// already concedes.
const FRAME: Duration = Duration::from_millis(16);

/// The batch ceiling. The pipe holds 1024 events, and one sink call is one
/// webview eval: a resumed pipe's full 1024 — each an entry record with a
/// tool-result payload — is a script the display cannot parse inside a frame,
/// which is the stall this pump exists to prevent. Past the ceiling the rest
/// ships on the next call at once — a full pipe drains call-after-call, so the
/// ceiling bounds one payload, never the delivery.
pub(super) const MAX_BATCH: usize = 64;

pub async fn pump(core: Arc<Core>, mut sink: impl FnMut(&[Event])) {
    let mut rx = core.events();
    let mut batch: Vec<Event> = Vec::new();
    while let Some(event) = rx.recv().await {
        batch.push(event);
        fill(&mut rx, &mut batch).await;
        sink(&batch);
        batch.clear();
    }
}

/// Fill one frame's batch: everything the pipe holds until the window closes,
/// the ceiling is reached, or the channel closes (a closed channel's buffered
/// events still reach `recv`, so the loop ends on its own afterwards).
async fn fill(rx: &mut mpsc::Receiver<Event>, batch: &mut Vec<Event>) {
    let due = tokio::time::Instant::now() + FRAME;
    while batch.len() < MAX_BATCH {
        match rx.try_recv() {
            Ok(event) => batch.push(event),
            Err(TryRecvError::Empty) => {
                let now = tokio::time::Instant::now();
                if now >= due {
                    return;
                }
                tokio::time::sleep(due - now).await;
            }
            Err(TryRecvError::Disconnected) => return,
        }
    }
}
