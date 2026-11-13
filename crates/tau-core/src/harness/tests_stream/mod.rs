//! The stream harness: sub-agent lifecycle, wakes, and the event stream a
//! running turn projects (#148, #175, #187, #226, #261, #368).
//!
//! A leg's setup is the same three moves every time — boot a core over a temp
//! workspace, open a parent session, spawn a scripted child — so that lives
//! here once (`stage`, `Stream::spawn`) and a leg is written as its assertions
//! (#375). A new leg costs its assertions, not the scaffold.

use super::*;

use crate::harness::testkit::*;
use serde_json::json;
use tau_protocol::{EntryKind, WaitingOn};

mod child;
mod projection;
mod wake;

/// The child's plan file content that declares the work complete: the observed
/// lifecycle derives `done` from it (spec §5.2), and the child's one-time
/// pre-delay (`CannedChild`) holds its first call while the leg seeds it.
const DONE_PLAN: &str = "# Plan\n\n## Steps\n\n- [x] the work\n";

/// The core over the canned provider set, its child replaying `body` once.
pub(crate) fn canned_child_core(body: String) -> Arc<Core> {
    CoreBuilder::custom(providers())
        .with_child_factory(Arc::new(CannedChildFactory { body }))
        .build()
        .unwrap()
}

/// The core over the canned provider set, its child sleeping `delay_ms`
/// between events: the turn stays in flight long enough to observe.
pub(crate) fn slow_child_core(
    providers: BTreeMap<String, crate::config::Provider>,
    body: String,
    delay_ms: u64,
) -> Arc<Core> {
    CoreBuilder::custom(providers)
        .with_child_factory(Arc::new(SlowChildFactory { body, delay_ms }))
        .build()
        .unwrap()
}

/// The core over one canned provider with no `with_child_factory`, so the
/// child runs on the default forwarding factory — the stash-reuse path #175 fixes.
pub(crate) fn canned_provider_core() -> Arc<Core> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "dev".into(),
        crate::config::Provider::with_model("canned://text", "model"),
    );
    CoreBuilder::custom(providers).build().unwrap()
}

/// The canned set at `slow-first`: its first call hangs in prefill, the
/// in-flight window a mid-turn send has to land inside (#148).
pub(crate) fn canned_slow_first_provider() -> BTreeMap<String, crate::config::Provider> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "dev".into(),
        crate::config::Provider::with_model("canned://slow-first", "model"),
    );
    providers
}

/// A leg's stage: the core, the temp dir the workspace is rooted in (kept
/// alive for the leg's whole scope — the store and plan paths point into it),
/// the workspace, one parent session on it, and the event stream collected
/// from the pump.
pub(crate) struct Stream {
    pub(crate) core: Arc<Core>,
    pub(crate) tmp: tempfile::TempDir,
    pub(crate) workspace: Workspace,
    pub(crate) session: SessionMeta,
    pub(crate) events: Arc<Mutex<Vec<Event>>>,
}

pub(crate) async fn stage(core: Arc<Core>) -> Stream {
    let events = collect_events(&core);
    let tmp = tempfile::tempdir().unwrap();
    let workspace = open_ws(&core, tmp.path()).await;
    let session = match core
        .dispatch(Command::SessionNew {
            workspace: workspace.id.clone(),
            title: None,
            base_prompt: None,
        })
        .unwrap()
    {
        CommandOutput::Session { session } => session,
        other => panic!("expected a session: {other:?}"),
    };
    Stream {
        core,
        tmp,
        workspace,
        session,
        events,
    }
}

impl Stream {
    /// Spawn one fresh-context `general` child on the parent session.
    pub(crate) fn spawn(&self, brief: &str) -> SubagentInfo {
        match self
            .core
            .dispatch(Command::SubagentSpawn {
                session: self.session.id.clone(),
                agent_type: "general".into(),
                brief: brief.into(),
                context_mode: ContextMode::Fresh,
            })
            .unwrap()
        {
            CommandOutput::Subagent { subagent } => subagent,
            other => panic!("expected a subagent: {other:?}"),
        }
    }

    /// Seed `child`'s plan file, the declaration the observed lifecycle reads.
    pub(crate) fn seed_plan(&self, child: &str, raw: &str) {
        std::fs::write(crate::plan::path(self.tmp.path(), child), raw).unwrap();
    }

    /// The session's own live state, as the GUI reads it.
    pub(crate) fn live_state(&self, session: &str) -> LiveState {
        match self
            .core
            .dispatch(Command::SessionSnapshot {
                session: session.into(),
            })
            .unwrap()
        {
            CommandOutput::Snapshot { snapshot } => snapshot.live,
            other => panic!("expected a snapshot: {other:?}"),
        }
    }

    /// The session's own turn flag, as the turn loop sets it.
    pub(crate) fn turn_running(&self, session: &str) -> bool {
        self.core
            .live(session)
            .is_ok_and(|l| l.turn.load(Ordering::SeqCst))
    }

    /// The parent's supervisor: the child-state probe (`child_running`).
    pub(crate) fn supervisor(&self) -> Arc<Supervisor> {
        self.core
            .live(&self.session.id)
            .expect("the parent session is live")
            .agent
            .subagents()
            .expect("the parent session owns a supervisor")
    }

    pub(crate) fn send(&self, session: &str, text: &str, lane: MessageLane) {
        self.core
            .dispatch(Command::MessageSend {
                session: session.into(),
                text: text.into(),
                lane,
            })
            .unwrap();
    }

    /// The durable record of `session` — what a dispatch appended to its file.
    pub(crate) fn entries(&self, session: &str) -> Vec<Entry> {
        let mut store = SessionStore::for_workspace(Path::new(&self.workspace.cwd), session);
        store.open().unwrap();
        store.entries_range(0, usize::MAX).unwrap()
    }
}

/// Bounded scan of the collected stream: `found` inspects everything seen so
/// far and returns what the leg needs. Generous on purpose (a red is a code
/// problem, never a slow runner); `why` names the leg's failure on the way out.
pub(crate) async fn scan<T>(
    events: &Arc<Mutex<Vec<Event>>>,
    secs: u64,
    mut found: impl FnMut(&[Event]) -> Option<T>,
    why: impl Fn(&[Event]) -> String,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let seen = events.lock().clone();
        if let Some(found) = found(&seen) {
            return found;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", why(&seen));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Bounded poll of a condition the stream does not carry: a snapshot's turn
/// flag, the supervisor's view of a child, the turn flag on the parent.
pub(crate) async fn poll(secs: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !done() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// The queue projection a `MessageSend` emits for `session`: the item's text
/// and sender, as the GUI's composer shows them.
fn queued(events: &[Event], session: &str, text: &str, source: &str) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            Event::Queue { session: s, items, .. }
                if s == session
                    && items.iter().any(|q| {
                        q.text == text && q.source.as_deref() == Some(source)
                    })
        )
    })
}

/// The steering report a child's wake leaves on the parent's branch,
/// attributed to the child (#148).
fn report_from(entries: &[Entry], child: &str) -> bool {
    entries.iter().any(|e| {
        e.kind == EntryKind::User
            && e.payload["lane"] == "steering"
            && e.payload["source"] == json!(child)
    })
}
