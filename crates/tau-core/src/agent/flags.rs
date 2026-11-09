//! The session's cut flags (5F): a force send's per-call `kill`, the
//! persistent `stop`, and the one-way `closed`. Each question they answer is
//! read by name here, so no use site spells the disjunction by hand.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

/// The stop-flag poll cadence: a human clicking stop, not a tight race.
const STOP_POLL_MS: u64 = 50;

/// A session's cut flags, cloned into a call's streaming sink so the live
/// stream and the turn loop read one set. The flags stay three: `kill` is
/// cleared at every fresh call and turn end, `stop` survives to the next send
/// (ticket #23), `closed` never clears.
#[derive(Clone)]
pub(crate) struct StopFlags {
    kill: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl StopFlags {
    /// The set of a new session. `stop` is the `Arc` the creating site minted:
    /// a child's is the same one its forwarding seam holds, so the seam that
    /// cuts the stream and the loop that records the interrupted entry read
    /// one flag (#2B/#2C).
    pub(crate) fn new(stop: Arc<AtomicBool>) -> Self {
        Self {
            kill: Arc::new(AtomicBool::new(false)),
            stop,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The persistent stop as a shareable handle, for the seams that share it.
    pub(crate) fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// A force send kills the in-flight stream (spec §7).
    pub(crate) fn set_kill(&self) {
        self.kill.store(true, Ordering::SeqCst);
    }

    /// A fresh call starts un-killed, and a cut turn is consumed at its end:
    /// the kill must not bleed into the stream that follows (spec §7).
    pub(crate) fn clear_kill(&self) {
        self.kill.store(false, Ordering::SeqCst);
    }

    /// The persistent stop lands, and cuts through the prefill window
    /// (ticket #23).
    pub(crate) fn set_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// A send clears a prior stop: a stop means "interrupt current work", not
    /// "never work again" (ticket #23).
    pub(crate) fn clear_stop(&self) {
        self.stop.store(false, Ordering::SeqCst);
    }

    /// The session is closed: a one-way transition, set by `SessionClose`.
    pub(crate) fn set_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// The stop alone, with no kill: what cancels the rest of a tool batch
    /// (#222) and pre-empts the drain of queued messages (#223). A force
    /// send's kill is deliberately not included — a force's message is queued
    /// to run, and its tool batch completes per config (spec §7).
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// The cut the user asked for: a force send's kill, or the stop
    /// (spec §7, #229).
    pub(crate) fn cut(&self) -> bool {
        self.kill.load(Ordering::SeqCst) || self.stop_requested()
    }

    /// The session is finished for the turn in flight: the user's cut, plus
    /// the one-way close. The segment it leaves is an `interrupted` entry.
    pub(crate) fn finished(&self) -> bool {
        self.cut() || self.closed.load(Ordering::SeqCst)
    }

    /// The prefill half of a cut (spec §7): resolves once the user's cut
    /// lands, so the provider tears the in-flight request down before the
    /// first stream event arrives.
    pub(crate) async fn cut_signal(&self) {
        // Polled because a prefill has no stream event to react to.
        while !self.cut() {
            tokio::time::sleep(Duration::from_millis(STOP_POLL_MS)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as Real;

    fn set() -> StopFlags {
        StopFlags::new(Arc::new(AtomicBool::new(false)))
    }

    /// A session that has asked for nothing has cut nothing.
    #[test]
    fn a_fresh_session_has_cut_nothing() {
        let flags = set();
        assert!(!flags.cut());
        assert!(!flags.finished());
        assert!(!flags.stop_requested());
    }

    /// Each cut flag is an arm of `cut`, and a cut session is finished.
    #[test]
    fn every_cut_flag_is_a_cut_and_a_finished_session() {
        let killed = set();
        killed.set_kill();
        assert!(killed.cut());
        assert!(killed.finished());

        let stopped = set();
        stopped.set_stop();
        assert!(stopped.cut());
        assert!(stopped.finished());
    }

    /// A force is not a stop: its tool batch runs and its message is queued.
    #[test]
    fn a_force_is_a_cut_but_never_a_stop() {
        let flags = set();
        flags.set_kill();
        assert!(flags.cut());
        assert!(!flags.stop_requested());
    }

    /// A close finishes the session without being a cut the user made.
    #[test]
    fn a_close_finishes_the_session_without_cutting_it() {
        let flags = set();
        flags.set_closed();
        assert!(!flags.cut());
        assert!(flags.finished());
    }

    /// The per-call kill is cleared for the next call; the persistent stop
    /// survives it, and a close outlives both.
    #[test]
    fn clearing_reaches_kill_and_stop_but_never_closed() {
        let flags = set();
        flags.set_kill();
        flags.set_stop();
        flags.clear_kill();
        assert!(flags.cut(), "the stop outlives the kill clear");
        flags.clear_stop();
        assert!(!flags.cut());
        assert!(!flags.finished());

        let closed = set();
        closed.set_stop();
        closed.set_closed();
        closed.clear_stop();
        assert!(!closed.cut(), "a send clears the stop");
        assert!(closed.finished(), "closed never clears");
    }

    /// The stop handle is the `Arc` the creating site minted: the seam that
    /// cuts a child's stream and the loop that records the entry read one
    /// flag (#2B/#2C).
    #[test]
    fn the_stop_handle_is_the_shared_flag() {
        let stop = Arc::new(AtomicBool::new(false));
        let flags = StopFlags::new(Arc::clone(&stop));
        flags.set_stop();
        assert!(stop.load(Ordering::SeqCst), "the creator sees the stop");
        assert!(Arc::ptr_eq(&stop, &flags.stop_flag()));
    }

    /// A cut lands in the prefill poll: the kill arm tears the request down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cut_signal_resolves_on_the_kill_flag() {
        let flags = set();
        let setter = flags.clone();
        let poll = flags.cut_signal();
        tokio::pin!(poll);
        assert!(
            tokio::time::timeout(Real::from_millis(10), poll.as_mut())
                .await
                .is_err(),
            "the poll resolved with no cut"
        );
        setter.set_kill();
        tokio::time::timeout(Real::from_millis(200), poll)
            .await
            .expect("the kill did not resolve the prefill poll");
    }

    /// A cut lands in the prefill poll: the stop arm tears the request down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cut_signal_resolves_on_the_stop_flag() {
        let flags = set();
        let setter = flags.clone();
        let poll = flags.cut_signal();
        tokio::pin!(poll);
        setter.set_stop();
        tokio::time::timeout(Real::from_millis(200), poll)
            .await
            .expect("the stop did not resolve the prefill poll");
    }

    /// A close is not a cut, so the prefill poll does not tear the request
    /// down on it — the stream runs to its own end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_close_leaves_the_prefill_poll_running() {
        let flags = set();
        flags.set_closed();
        assert!(
            tokio::time::timeout(Real::from_millis(200), flags.cut_signal())
                .await
                .is_err(),
            "a close is not a cut"
        );
    }
}
