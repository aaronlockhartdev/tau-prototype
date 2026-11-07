use super::states::{ChildState, ContextMode, WaitingOn};
use crate::agent::AgentSession;
use crate::provider::TurnProviderRef;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tau_protocol::payload::ResumeContract;

/// The provider a child's loop runs against, created per child session id
/// (the dispatch implements this to wrap the production provider in its
/// stream-forwarding seam; tests return canned providers). `model` is the
/// child's model id, so the factory can route the child to the provider that
/// id names (#336) instead of the one its parent happens to address.
///
/// `stop` is the child's one stop flag: the spawn mints it and hands it to
/// this provider and to the child's agent, so the seam's stream cut, its
/// prefill `stop_signal`, and the loop's interrupted entry all read one flag
/// (#2B/#2C).
///
/// Stateless: every fact the child needs crosses the spawn notice
/// (`SpawnNotice`), so a factory holds no per-child state between `create`
/// and the bridge's registration — the provider it returns is the one the
/// child's live session is registered with (#175).
pub trait ChildProviderFactory: Send + Sync {
    fn create(
        &self,
        child_session_id: &str,
        model: &str,
        stop: &Arc<AtomicBool>,
    ) -> TurnProviderRef;
}

/// The parent-side notice for a child state transition: the GUI badge and
/// snapshot refresh. Idempotent-cumulative — the full state of one handle.
#[derive(Clone, Debug, PartialEq)]
pub struct StateNotice {
    pub parent: String,
    pub handle: String,
    pub child: String,
    pub state: ChildState,
    pub note: Option<String>,
    /// A stopped child's resume contract (the plan's next action, spec
    /// §5.2); None for every other transition.
    pub resume_contract: Option<ResumeContract>,
}

/// A spawn notice (the protocol's `subagent_spawned` event). It carries every
/// fact the spawn has in hand at the moment it mints the notice — the child's
/// agent, the provider the factory built with the child's stop flag, its
/// title, cwd and created stamp — so the bridge registers what it is handed
/// and resolves nothing back from a registry, a store re-open, or the handle.
#[derive(Clone)]
pub struct SpawnNotice {
    pub parent: String,
    pub handle: String,
    pub child: String,
    pub agent_type: String,
    pub context_mode: ContextMode,
    /// The child's model (the bridge registers the child's live session with it).
    pub model: String,
    /// The child's session title, minted by the spawn (the adjective-noun pair
    /// it wrote into the header): the GUI's stub row gets the real name.
    pub title: String,
    /// The child's cwd — its parent's, the spawn's own.
    pub cwd: std::path::PathBuf,
    /// The child session's created stamp (epoch ms).
    pub created: u64,
    /// The child's agent, built by the spawn.
    pub agent: Arc<AgentSession>,
    /// The provider the spawn built with the agent's stop flag: the same
    /// object the child's agent calls through (#175), registered as the live
    /// session's provider.
    pub provider: TurnProviderRef,
}

// identity-only debug: `agent` and `provider` are not Debug
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for SpawnNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnNotice")
            .field("parent", &self.parent)
            .field("handle", &self.handle)
            .field("child", &self.child)
            .field("agent_type", &self.agent_type)
            .field("context_mode", &self.context_mode)
            .field("model", &self.model)
            .field("title", &self.title)
            .field("cwd", &self.cwd)
            .field("created", &self.created)
            .finish()
    }
}

/// A wake (ADR-0001 wake rules): the implementation appends the
/// notification to the parent's active branch (child provenance) and starts
/// a turn there.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeNotice {
    pub parent: String,
    pub child: String,
    pub kind: WakeKind,
    pub text: String,
    pub waiting_on: Option<WaitingOn>,
    pub output: Option<Value>,
}

/// What woke the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeKind {
    /// done — the parent is woken always.
    Done,
    /// failed — the parent is notified always (to investigate).
    Failed,
    /// idle with `waiting_on: parent` — a parked parent is woken with the
    /// child's stated need.
    Waiting,
    /// stopped — the parent is told the child was stopped (the note names
    /// who), so its model sees the child as stopped, not running.
    Stopped,
}

/// The parent-side seam the supervisor calls across (the dispatch
/// implements it: protocol events + the wake).
pub trait SubagentBridge: Send + Sync {
    fn spawned(&self, notice: &SpawnNotice);
    fn state(&self, notice: &StateNotice);
    fn wake(&self, notice: &WakeNotice);
}

/// A drive's result: `Err` = the child's loop failed (provider/storage) →
/// the child fails and the parent is notified.
pub type BoxedDrive = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// The child turn driver (the dispatch implements it): run the child's
/// queue to completion (the turns) plus the post-turn work, and return
/// when the queue drains.
pub trait ChildDriver: Send + Sync {
    fn drive(&self, session: &str, agent: &Arc<AgentSession>) -> BoxedDrive;
}
