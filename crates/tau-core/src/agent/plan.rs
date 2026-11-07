//! The session's plan state (design: the plan file is the source of truth).
//! The session observes its own file — the plan watcher's batch, and the
//! session start — and the parse lands here, so the state lives with the
//! session it describes: a closed session keeps none, and a re-open's
//! observation runs before any listener is wired, so the start is silent
//! (a plain open must not advance the sidebar's last-modified, #283).

use super::{AgentSession, PlanObserver};

impl AgentSession {
    /// Re-read the plan file into the session's state: the observation. A
    /// failed read (a torn write, a delete) degrades to an empty plan — the
    /// parser is total, so nothing here can fail a turn. The parse reaches
    /// the observer only when it differs from the last one: an editor saving
    /// identical content must not churn the plan pane (#177).
    pub(crate) fn observe_plan(&self) {
        let (file, session) = {
            let inner = self.inner.lock();
            (
                crate::plan::path(&inner.cwd, inner.store.id()),
                inner.store.id().to_owned(),
            )
        };
        let raw = std::fs::read_to_string(&file).unwrap_or_default();
        let plan = crate::plan::parse(&raw);
        let (changed, observer) = {
            let mut inner = self.inner.lock();
            let changed = inner.plan.as_ref().is_none_or(|prev| *prev != plan);
            inner.plan = Some(plan.clone());
            (changed, inner.plan_observer.clone())
        };
        tracing::info!(
            session = %session,
            done = plan.done_count(),
            total = plan.steps.len(),
            "plan state observed"
        );
        if changed && let Some(observer) = observer {
            observer(plan);
        }
    }

    /// The rendered plan state the assembly re-injects at each turn boundary
    /// and after compaction; `None` until the session's first observation.
    pub(crate) fn plan_state(&self) -> Option<String> {
        self.inner.lock().plan.as_ref().map(crate::plan::render)
    }

    /// The parsed plan state (the snapshot's plan pane); `None` until the
    /// session's first observation.
    pub(crate) fn plan(&self) -> Option<crate::plan::Plan> {
        self.inner.lock().plan.clone()
    }

    /// The change emitter (the harness wires it to the event channel);
    /// `None` clears it.
    pub(crate) fn set_plan_observer(&self, observer: Option<PlanObserver>) {
        self.inner.lock().plan_observer = observer;
    }
}
