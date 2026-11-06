//! Core state: the owned registry (workspaces, live sessions), the builder, the workspace index, and the small shared helpers.

use super::{
    AgentSession, Arc, AtomicBool, AtomicU64, BTreeMap, ChildProviderFactory, Config,
    ConfigRejection, ConfigState, Duration, Event, HashMap, Lane, MessageLane, Mutex, Ordering,
    Path, PathBuf, ProtocolError, SessionMeta, Skeleton, SkillInfo, SystemEventKind,
    TurnProviderRef, Watcher, Weak, Workspace, config, mpsc, skill_info,
};

/// The watcher's debounce window (design #30): a save burst (a temp-write
/// followed by an atomic rename) ends well inside it; downstream work (a
/// few directory reads, depth ≤ 4) is trivial, so there is nothing to
/// gain from a longer window, and 500 ms reads as instant in the GUI.
pub(crate) const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);
/// A title's cap: the archive listing reads the header line bounded
/// (4 KiB), so a title must stay far below that — an oversized title
/// would overflow the read and the entry would silently drop from the
/// archive list, losing its only restore row (review N3).
pub(crate) const MAX_TITLE_LEN: usize = 200;

/// The client's connect-phase timeout (applied to the shared client; the
/// streaming body is bounded per provider by `requests.timeout_secs` as a
/// per-chunk idle deadline). A connect that takes this long is a dead
/// endpoint, not a slow model.
pub(crate) const CONNECT_TIMEOUT_SECS: u64 = 30;

/// A live session: the loop plus the binding's view of its lanes, the
/// stop flag, and the provider (kept here so a turn can be diffed against
/// the calls it started). The GUI's queue is not held here: it is a
/// projection of the agent's own lane queue, taken at emit time.
pub(crate) struct LiveSession {
    pub(crate) meta: Mutex<SessionMeta>,
    pub(crate) agent: Arc<AgentSession>,
    /// User stop (spec §7): the forwarding sink returns false on it, which
    /// cuts the in-flight stream the way a force does — a stop is a force
    /// with no message.
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) turn: AtomicBool,
    /// The provider the loop runs against, and the surface the post-turn
    /// reconciliation reads the turn's calls from (`TurnProvider::call_log`).
    /// A child's is the provider its agent has — one object per child (#175).
    pub(crate) provider: TurnProviderRef,
    pub(crate) cwd: PathBuf,
    /// The snapshot skeleton this session's polls reuse (#316); see
    /// `super::Skeleton`.
    pub(crate) skeleton: Mutex<Skeleton>,
}

/// One open workspace and everything that travels with it (#1A): a fact about a
/// workspace lives here, so `close_workspace`'s one `remove` is the whole
/// teardown — nothing of a closed workspace survives, and `Drop`'s one `clear`
/// stops every watcher (a `Watcher` stops on drop).
pub(crate) struct WorkspaceState {
    pub(crate) workspace: Workspace,
    /// The `.tau/skills` + `.agents/skills` roots (#31).
    pub(crate) project_watcher: Option<Watcher>,
    /// The `.tau` config layer (#337).
    pub(crate) config_watcher: Option<Watcher>,
    /// The files pane's watcher, on the dirs the pane expanded (#32/#115).
    pub(crate) tree_watcher: Option<Watcher>,
    /// The `.tau/plans` root.
    pub(crate) plan_watcher: Option<Watcher>,
    /// The discovered skill registry, refreshed by the watchers (#28/#31).
    pub(crate) skills: Option<Vec<SkillInfo>>,
    pub(crate) git: GitCache,
}

/// A workspace's git snapshot (file-tree plan, phase 2): `NotGit` is the cached
/// negative, so a non-git workspace pays three spawns once, not three a listing;
/// `Uncomputed` is what a tree change returns the cache to.
#[derive(Clone, Default)]
pub(crate) enum GitCache {
    #[default]
    Uncomputed,
    NotGit,
    Snapshot(Box<super::git::GitSnapshot>),
}

impl GitCache {
    /// What a listing's fresh answer leaves behind: the snapshot, or the cached
    /// negative where the spawn failed (the next listing reads, no spawns).
    pub(crate) fn from_snapshot(snap: Option<super::git::GitSnapshot>) -> Self {
        match snap {
            Some(snap) => GitCache::Snapshot(Box::new(snap)),
            None => GitCache::NotGit,
        }
    }
}

impl WorkspaceState {
    fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            project_watcher: None,
            config_watcher: None,
            tree_watcher: None,
            plan_watcher: None,
            skills: None,
            git: GitCache::Uncomputed,
        }
    }
}

/// The core's owned state: the open workspaces with the state that travels with
/// each, and the live sessions. Everything the GUI can show is derivable from
/// here + the session files (ADR-0006 state ownership).
pub struct Core {
    pub(crate) workspaces: Mutex<BTreeMap<String, WorkspaceState>>,
    pub(crate) sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    /// Each workspace's config state: the config it runs on, and the load
    /// failure it runs beside (#359). The system layer is not a workspace, so
    /// no key in this map borrows a sentinel id — it lives in `system`.
    pub(crate) configs: Mutex<HashMap<String, ConfigState>>,
    /// The system layer's state: the config every workspace's merge rides, and
    /// the cause a cold read folds in when that layer stops reading (#364,
    /// #419). One value, not a map keyed by the empty workspace id.
    pub(crate) system: Mutex<ConfigState>,
    /// The home-level skill roots (`{system}/skills` + `{home}/.agents/skills`),
    /// watched once globally (ticket #31): identical for every workspace, so
    /// one watcher re-discovers all open workspaces. A `None` seam (the test
    /// shape) is a no-op — there are no roots to watch.
    pub(crate) home_watcher: Mutex<Option<Watcher>>,
    /// The system config layer (`{system}/config.toml`), watched once globally
    /// (#337): the layer is shared by every workspace, so one watcher reloads
    /// it for all. Watched non-recursively — the dir's other files
    /// (`workspaces.json`, `agents`, `skills`) are not config.
    pub(crate) system_config_watcher: Mutex<Option<Watcher>>,
    /// The watcher-parsed plan state per session (design: the plan file is
    /// the source of truth; this is its parse, kept fresh by the
    /// per-workspace plan watcher). Dropped on session close — the file on
    /// disk is the record, the cache is not.
    pub(crate) plans: Mutex<HashMap<String, crate::plan::Plan>>,
    pub(crate) system_dir: Option<PathBuf>,
    /// The user's home dir (the home-level `.agents/skills/` root; the
    /// `system_dir` seam's sibling for tests).
    pub(crate) home: Option<PathBuf>,
    pub(crate) custom: bool,
    /// Providers layered via `CoreBuilder::with_provider` (the ACP env
    /// channel): re-applied in `workspace_config` after the project layer,
    /// so a workspace's file config can never shadow them.
    pub(crate) explicit_providers: BTreeMap<String, crate::config::Provider>,
    pub(crate) client: reqwest::Client,
    pub(crate) events_tx: mpsc::Sender<Event>,
    /// The event pipe's drop bookkeeping (never silent: a drop is counted
    /// and a summary error is delivered at the next successful send).
    pub(crate) pipe: PipeCounters,
    /// The pump's half of the events channel; taken exactly once.
    pub(crate) events_rx: Mutex<Option<mpsc::Receiver<Event>>>,
    /// The test-seam child provider factory (None in production builds).
    pub(crate) child_factory: Option<Arc<dyn ChildProviderFactory>>,
    /// The test-seam root provider: when set, the `SessionNew`/reopen path
    /// binds this to the root session instead of the config's first provider
    /// (None in production builds), so a test can capture the re-injected
    /// instructions through the production wiring.
    pub(crate) root_provider: Option<TurnProviderRef>,
    /// Child handle → parent session (the spawn's record, R3): the handle
    /// is opaque — a command carrying it resolves the parent from here,
    /// never from the handle's format.
    pub(crate) child_handles: Mutex<HashMap<String, String>>,
    /// Self-reference for the seams that need an `Arc<Core>` (the child
    /// driver/factory/bridge); set in `build`.
    pub(crate) self_weak: Mutex<Option<Weak<Core>>>,
}

/// Core-construction failure: an OS-level condition with no in-band
/// recovery (an unset `HOME`, an HTTP-client or watcher-thread build
/// failure) — the embedding process exits with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreError(pub String);

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CoreError {}

pub struct CoreBuilder {
    system_dir: Option<PathBuf>,
    home: Option<PathBuf>,
    /// A custom root carries an explicit provider list (no system layer);
    /// a production root gets full file-level layering (spec §12).
    custom: bool,
    providers: BTreeMap<String, crate::config::Provider>,
    /// A test seam: the child provider factory replaces the production
    /// one (tests script child turns).
    child_factory: Option<Arc<dyn ChildProviderFactory>>,
    /// A test seam: the root provider the `SessionNew`/reopen path binds to
    /// the root session (tests capture the re-injected instructions through
    /// the production wiring).
    root_provider: Option<TurnProviderRef>,
}

impl CoreBuilder {
    /// Production shape: the system config dir (`~/.config/tau`).
    pub fn default_system() -> Result<Self, CoreError> {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Err(CoreError(
                "home is not set; cannot locate the system config dir".into(),
            ));
        };
        Ok(Self {
            system_dir: Some(home.join(".config").join("tau")),
            home: Some(home),
            custom: false,
            providers: BTreeMap::new(),
            child_factory: None,
            root_provider: None,
        })
    }

    /// A test shape: no config files, an explicit provider list.
    #[must_use]
    pub fn custom(providers: BTreeMap<String, crate::config::Provider>) -> Self {
        Self {
            system_dir: None,
            home: None,
            custom: true,
            providers,
            child_factory: None,
            root_provider: None,
        }
    }

    /// Layer one explicit provider on top of the loaded config (issue #66:
    /// the ACP env-override channel). Merged in `apply_startup` after the
    /// file layers, so it wins over a same-named file entry.
    #[must_use]
    pub fn with_provider(mut self, name: String, provider: crate::config::Provider) -> Self {
        self.providers.insert(name, provider);
        self
    }

    /// A test seam: child sessions get this factory's provider (scripted
    /// child turns).
    #[must_use]
    pub fn with_child_factory(mut self, f: Arc<dyn ChildProviderFactory>) -> Self {
        self.child_factory = Some(f);
        self
    }
    /// A test seam: the root session (the `SessionNew`/reopen path) binds
    /// this provider instead of the config's first provider, so a test can
    /// capture the re-injected instructions through the production wiring.
    #[must_use]
    pub fn with_root_provider(mut self, p: TurnProviderRef) -> Self {
        self.root_provider = Some(p);
        self
    }

    /// A test seam: a custom system dir (the workspaces.json location).
    #[must_use]
    pub fn with_system_dir(mut self, dir: PathBuf) -> Self {
        self.system_dir = Some(dir);
        self
    }

    /// A test seam: a custom home (the home-level `.agents/skills/` root).
    #[must_use]
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
        self
    }

    pub fn build(self) -> Result<Arc<Core>, CoreError> {
        let (events_tx, rx) = mpsc::channel(1024);
        let core = Arc::new(Core {
            pipe: PipeCounters {
                dropped: Arc::new(AtomicU64::new(0)),
                overflow_pending: Arc::new(AtomicBool::new(false)),
            },
            self_weak: Mutex::new(None),
            workspaces: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(HashMap::new()),
            configs: Mutex::new(HashMap::new()),
            system: Mutex::new(ConfigState::Current {
                config: Config::default(),
                rejection: None,
            }),
            home_watcher: Mutex::new(None),
            system_config_watcher: Mutex::new(None),
            plans: Mutex::new(HashMap::new()),
            system_dir: self.system_dir.clone(),
            home: self.home.clone(),
            custom: self.custom,
            explicit_providers: self.providers.clone(),
            // Test builds use a no-pool client: a pooled keep-alive connection
            // keeps the tokio runtime alive after the test, hanging teardown.
            client: {
                let mut builder = reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS));
                if self.custom {
                    builder = builder.pool_max_idle_per_host(0);
                }
                builder
                    .build()
                    .map_err(|e| CoreError(format!("http client build failed: {e}")))?
            },
            events_tx,
            events_rx: Mutex::new(Some(rx)),
            child_factory: self.child_factory.clone(),
            root_provider: self.root_provider.clone(),
            child_handles: Mutex::new(HashMap::new()),
        });
        core.self_weak.lock().replace(Arc::downgrade(&core));
        self.apply_startup(&core)?;
        Ok(core)
    }

    fn apply_startup(self, core: &Arc<Core>) -> Result<(), CoreError> {
        let mut config = Config::default();
        if let Some(dir) = self.system_dir {
            match config::load(&dir, Path::new(config::NO_PROJECT_LAYER)) {
                Ok(loaded) => config = loaded,
                Err(e) => {
                    // The app boots on defaults — the one place a bad config is
                    // allowed to ride — and the boot says so, before any session
                    // exists to record it. The first session under this layer
                    // reads the file itself and adds the transcript fact; the
                    // watcher started below turns a later edit into both (#363).
                    core.emit_config_rejected("", &e);
                    // The mark every cold read folds in. A workspace with no
                    // project layer reads the kept defaults and would see no
                    // cause; the session-open headline chains this mark, so a
                    // rejected layer is never reported as "no providers"
                    // (#419).
                    core.system
                        .lock()
                        .mark_stale(Core::rejection_at(&dir.join(config::CONFIG_FILE), &e));
                }
            }
        }
        for (name, p) in self.providers {
            config.providers.insert(name, p);
        }
        // The layer the boot read (defaults where it refused): a mark the arm
        // above wrote rides the swap, so the cold read still meets the cause.
        core.system.lock().set_current(config);
        // The workspaces a previous run left open: re-register the ones
        // whose folder still exists (sessions are read from disk on
        // demand); a closed workspace (B7) stays closed across the restart.
        for e in core.load_workspace_index() {
            if e.open && Path::new(&e.cwd).is_dir() {
                let state = WorkspaceState::new(Workspace {
                    id: e.id,
                    name: e.name,
                    cwd: e.cwd,
                });
                core.workspaces
                    .lock()
                    .entry(state.workspace.id.clone())
                    .or_insert(state);
            }
        }
        // The home-level roots are watched once globally (ticket #31); a
        // `None` seam (the test shape) is a no-op. The system config layer is
        // watched once for the same reason: every workspace merges it (#337).
        core.start_home_watcher()?;
        core.start_system_config_watcher()?;
        Ok(())
    }
}

/// A readable session name: adjective-noun from the `names` crate's
/// dictionaries, lowercase and hyphenated ("rusty-nail").
pub(crate) fn session_name() -> String {
    names::Generator::default()
        .next()
        .expect("the generator yields a name")
}

/// Draw a session name that is fresh against the workspace's existing
/// titles; after eight colliding draws (the dictionary is effectively
/// exhausted) the name is numbered until it is unique (note 8). The draw
/// function is a parameter so the fallback is testable without the
/// randomness.
pub(crate) fn unique_name(titles: &[String], mut draw: impl FnMut() -> String) -> String {
    let mut name = draw();
    for _ in 0..8 {
        if !titles.contains(&name) {
            return name;
        }
        name = draw();
    }
    let mut n = 2u32;
    loop {
        let suffixed = format!("{name} {n}");
        if !titles.contains(&suffixed) || n >= 100 {
            return suffixed;
        }
        n += 1;
    }
}

/// The workspace index (`{system dir}/workspaces.json`): the folders this
/// machine has opened; restored at boot so a restart reopens the world and
/// its sessions.
#[derive(serde::Serialize, serde::Deserialize)]
struct WorkspaceIndexEntry {
    id: String,
    name: String,
    cwd: String,
    /// The tab's open flag (B7): a closed workspace stays closed across a
    /// restart. Entries written before the flag existed default to open.
    #[serde(default = "open_default")]
    open: bool,
}
const fn open_default() -> bool {
    true
}
/// The routing pair an overflow notification rides on: the dropped
/// event's own workspace/session, insofar as it has one.
fn event_route(event: &Event) -> (String, Option<String>) {
    match event {
        Event::System {
            workspace, session, ..
        } => (workspace.clone(), session.clone()),
        Event::SkillListChanged { workspace, .. } | Event::FileTreeChanged { workspace, .. } => {
            (workspace.clone(), None)
        }
        Event::StreamStart {
            workspace, session, ..
        }
        | Event::StreamDelta {
            workspace, session, ..
        }
        | Event::StreamEnd {
            workspace, session, ..
        }
        | Event::EntryUpsert {
            workspace, session, ..
        }
        | Event::Queue {
            workspace, session, ..
        }
        | Event::SessionEvent {
            workspace, session, ..
        }
        | Event::OmStatus {
            workspace, session, ..
        }
        | Event::SubagentEvent {
            workspace, session, ..
        }
        | Event::PlanChanged {
            workspace, session, ..
        } => (workspace.clone(), Some(session.clone())),
    }
}

/// The event pipe's drop bookkeeping, shared by the core and every
/// forwarding sink: the drop counter (the ground truth) and the
/// not-yet-delivered overflow summary.
#[derive(Clone)]
pub(crate) struct PipeCounters {
    /// Total events dropped at the pipe's boundary.
    pub(crate) dropped: Arc<AtomicU64>,
    /// An overflow happened whose summary has not been delivered yet.
    pub(crate) overflow_pending: Arc<AtomicBool>,
}

/// Send one event to the GUI, surfacing drops: an overflow (the bounded
/// channel full) is counted and owes a summary. The summary cannot ride
/// the same full channel, so it is delivered by the next *successful*
/// send — the first moment the pipe has room again.
pub(crate) fn pipe_send(
    tx: &mpsc::Sender<Event>,
    counters: &PipeCounters,
    event: Event,
    workspace: String,
    session: Option<String>,
) {
    if let Ok(()) = tx.try_send(event) {
        if counters.overflow_pending.swap(false, Ordering::Relaxed) {
            let total = counters.dropped.load(Ordering::Relaxed);
            let summary = Event::System {
                workspace,
                session,
                kind: SystemEventKind::Error {
                    message: format!(
                        "event pipe overflow: {total} event(s) dropped — the view may be missing events; reload"
                    ),
                },
            };
            // The summary can miss too (the channel refills in the
            // same instant): then it stays owed.
            if tx.try_send(summary).is_err() {
                counters.overflow_pending.store(true, Ordering::Relaxed);
            }
        }
    } else {
        counters.dropped.fetch_add(1, Ordering::Relaxed);
        counters.overflow_pending.store(true, Ordering::Relaxed);
    }
}
impl Core {
    pub fn events(&self) -> mpsc::Receiver<Event> {
        self.events_rx
            .lock()
            .take()
            .expect("the event pump takes the receiver exactly once")
    }

    pub(crate) fn emit(&self, event: Event) {
        let (workspace, session) = event_route(&event);
        pipe_send(&self.events_tx, &self.pipe, event, workspace, session);
    }

    /// The core as an `Arc` (the child seams keep one); None pre-`build`.
    pub(crate) fn self_arc(&self) -> Option<Arc<Core>> {
        self.self_weak.lock().as_ref().and_then(Weak::upgrade)
    }

    pub(crate) fn system_config(&self) -> Config {
        self.system.lock().config().cloned().unwrap_or_default()
    }

    pub(crate) fn open_workspace(self: &Arc<Self>, cwd: &str) -> Result<Workspace, ProtocolError> {
        let cwd = cwd.trim().to_owned();
        let id = format!("w-{:x}", xxhash_rust::xxh3::xxh3_64(cwd.as_bytes()));
        let name = Path::new(&cwd)
            .components()
            .next_back()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cwd.clone());
        let workspace = Workspace { id, name, cwd };
        self.workspaces
            .lock()
            .entry(workspace.id.clone())
            .or_insert_with(|| WorkspaceState::new(workspace.clone()));
        self.emit(Event::System {
            workspace: workspace.id.clone(),
            session: None,
            kind: SystemEventKind::WorkspaceOpened {
                name: workspace.name.clone(),
                cwd: workspace.cwd.clone(),
            },
        });
        self.set_workspace_open(&workspace, true);
        // A watcher build failure (OS thread exhaustion) refuses the open
        // instead of leaving the workspace unwatched.
        let refuse = |e: CoreError| ProtocolError::Other {
            message: e.to_string(),
        };
        self.start_project_watcher(&workspace).map_err(&refuse)?;
        self.start_config_watcher(&workspace).map_err(&refuse)?;
        self.start_tree_watcher(&workspace).map_err(&refuse)?;
        self.start_plan_watcher(&workspace).map_err(&refuse)?;
        Ok(workspace)
    }

    pub(crate) fn workspace(&self, id: &str) -> Result<Workspace, ProtocolError> {
        self.workspaces
            .lock()
            .get(id)
            .map(|state| state.workspace.clone())
            .ok_or_else(|| ProtocolError::NotFound {
                what: format!("workspace {id} is not open"),
            })
    }

    /// Every open workspace's identity: the fan-out reads that walk all of them
    /// (the home watcher's batch, a system layer's reload, the archive's and
    /// delete's disk scans for a closed session).
    pub(crate) fn open_workspaces(&self) -> Vec<Workspace> {
        self.workspaces
            .lock()
            .values()
            .map(|state| state.workspace.clone())
            .collect()
    }

    /// The open workspace `test` picks out of the live set (the archive and
    /// delete paths find a closed session by the file on disk under its cwd).
    pub(crate) fn find_workspace(&self, test: impl Fn(&Workspace) -> bool) -> Option<Workspace> {
        self.open_workspaces().into_iter().find(|w| test(w))
    }

    /// The workspace's skill registry, read from its own state (#28): discovered
    /// outlives a change. The one read site, so the cache cannot be missed on the
    /// way in. A cold slot runs the producer and answers with what it built, so
    /// discovery has one path: the reader never walks the skill dirs on its own
    /// and never re-reads the key the writer just filled. Discovery runs with
    /// `workspaces` free — a first read walks the skill dirs, and a workspace list
    /// should not queue behind it. A workspace with no state (the cwd closed under
    /// this lookup) answers from the files: the cache is the optimization,
    /// discovery is the record.
    pub(crate) fn skills_of(&self, ws: &Workspace) -> Vec<SkillInfo> {
        if let Some(reg) = self
            .workspaces
            .lock()
            .get(&ws.id)
            .and_then(|state| state.skills.clone())
        {
            return reg;
        }
        self.refresh_skills(ws).iter().map(skill_info).collect()
    }

    /// Re-run discovery, replace the workspace's registry slot, and emit the
    /// full-state `SkillListChanged` (#31 — idempotent by construction, a lost
    /// batch self-heals on the next `skill_list`). Shared by the watchers,
    /// `build_live`, and the cold read in `skills_of`: the slot is the one the
    /// dropdown and the `/skill:` lookup read. A running session's prompt is not
    /// touched (frozen at build); new sessions pick up the change at their own
    /// build. Returns the `Skill`s, not the rows: launch's prompt catalog reads
    /// the bodies the rows drop.
    pub(crate) fn refresh_skills(&self, ws: &Workspace) -> Vec<crate::skills::Skill> {
        let skills = crate::skills::discover(
            self.system_dir.as_deref(),
            self.home.as_deref(),
            Path::new(&ws.cwd),
        );
        let reg: Vec<SkillInfo> = skills.iter().map(skill_info).collect();
        if let Some(state) = self.workspaces.lock().get_mut(&ws.id) {
            state.skills = Some(reg.clone());
        }
        self.emit(Event::SkillListChanged {
            workspace: ws.id.clone(),
            skills: reg,
        });
        skills
    }

    /// The open workspace whose cwd is `path` (the plan event's routing);
    /// `None` when the cwd names no open workspace (a close racing the
    /// watcher) — the parse is cached either way.
    fn workspace_id_for_cwd(&self, cwd: &Path) -> Option<String> {
        self.workspaces
            .lock()
            .values()
            .find(|state| Path::new(&state.workspace.cwd) == cwd)
            .map(|state| state.workspace.id.clone())
    }

    fn load_workspace_index(&self) -> Vec<WorkspaceIndexEntry> {
        let Some(dir) = &self.system_dir else {
            return Vec::new();
        };
        let Ok(raw) = std::fs::read_to_string(dir.join("workspaces.json")) else {
            return Vec::new();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    fn save_workspace_index(&self, entries: &[WorkspaceIndexEntry]) {
        let Some(dir) = &self.system_dir else {
            return;
        };
        if let Ok(raw) = serde_json::to_vec_pretty(entries) {
            // Note 7: a torn plain write costs every workspace tab on the
            // next boot; the temp-file rename makes the index all-or-nothing.
            let _ = std::fs::create_dir_all(dir);
            let path = dir.join("workspaces.json");
            let nanos = crate::clock::now_ns();
            let tmp = dir.join(format!("workspaces.json.tmp-{nanos:016x}"));
            if std::fs::write(&tmp, raw).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    /// Set the workspace's `open` flag in the index (B7): true on open,
    /// false on close. The entry is created if absent (a first open, or a
    /// close racing ahead of any saved open) so the flag always sticks.
    fn set_workspace_open(&self, w: &Workspace, open: bool) {
        let mut entries = self.load_workspace_index();
        if let Some(e) = entries.iter_mut().find(|e| e.cwd == w.cwd) {
            e.open = open;
        } else {
            entries.push(WorkspaceIndexEntry {
                id: w.id.clone(),
                name: w.name.clone(),
                cwd: w.cwd.clone(),
                open,
            });
            entries.sort_by(|a, b| a.cwd.cmp(&b.cwd));
        }
        self.save_workspace_index(&entries);
    }

    /// Close (archive) a workspace (B7): persist its `open` flag as closed, then
    /// drop its state — the live entry, its watchers, and its caches are one
    /// thing, and a `workspace_list` that still carried it would resurrect the
    /// just-closed tab on the next sync. The sessions stay on disk; the workspace
    /// re-opens. The config entry goes on the same rule: a reopen of the same cwd
    /// meets the files as they are, not as the last session left them (#359).
    pub(crate) fn close_workspace(&self, id: &str) -> Result<(), ProtocolError> {
        let w = self.workspace(id)?;
        self.set_workspace_open(&w, false);
        self.workspaces.lock().remove(id);
        // One entry per workspace: the config it ran on and the mark beside it
        // go together.
        self.configs.lock().remove(id);
        Ok(())
    }

    /// The session's current plan state, rendered for injection (the
    /// watcher-parsed state of the plan file); `None` until the first
    /// observation.
    pub(crate) fn plan_state(&self, session: &str) -> Option<String> {
        self.plans.lock().get(session).map(crate::plan::render)
    }

    /// Re-read and parse the session's plan file into the state cache: the
    /// plan watcher's consumer, and the session-start seed. A failed read
    /// (a torn write, a delete) degrades to an empty plan — the parser is
    /// total, so nothing here can fail a turn. A changed parse is emitted as
    /// a `plan_changed` (the GUI's plan pane, spec §5.3) when the cwd names
    /// an open workspace — and only when the parse actually changed: an
    /// editor saving unchanged content must not churn the UI (#177).
    /// `silent` seeds the cache without emitting: the session-open path must
    /// warm the cache for the first assembly, but the seed is not a change —
    /// emitting it made a plain open advance the sidebar's last-modified.
    pub(crate) fn refresh_plan(&self, workspace: &Path, session_id: &str, silent: bool) {
        let raw =
            std::fs::read_to_string(crate::plan::path(workspace, session_id)).unwrap_or_default();
        let plan = crate::plan::parse(&raw);
        let done = plan.done_count();
        let total = plan.steps.len();
        let changed = self
            .plans
            .lock()
            .insert(session_id.to_owned(), plan.clone())
            .is_none_or(|prev| prev != plan);
        tracing::info!(
            session = %session_id,
            done,
            total,
            "plan state observed"
        );
        if silent || !changed {
            return;
        }
        if let Some(id) = self.workspace_id_for_cwd(workspace) {
            self.emit(Event::PlanChanged {
                workspace: id,
                session: session_id.to_owned(),
                plan: plan.into(),
            });
        }
    }

    /// The project layer's directory: where a workspace's own config file lives.
    /// Reading it is the merge rule; a load rule's failure names no file here (#366).
    pub(crate) fn project_layer_dir(workspace: &Workspace) -> PathBuf {
        Path::new(&workspace.cwd).join(".tau")
    }
    /// The workspace's config from its files, the merge rule the cache fill
    /// and the live reload share (#337): the system layer, then the project
    /// layer over it — `config::load` takes the project *directory* and joins
    /// `CONFIG_FILE` onto it, so a path to the file itself would miss (spec
    /// §12). A layer that fails to parse names no merged config, so the
    /// caller keeps what it has — the reload's failure path.
    pub(crate) fn read_workspace_config(
        &self,
        workspace: &Workspace,
    ) -> Result<Config, config::LoadError> {
        let project = Self::project_layer_dir(workspace);
        if self.custom {
            let mut c = self.system_config();
            // Custom roots carry explicit providers: a project entry replaces
            // the same-named root entry, others coexist.
            if project.exists() {
                let loaded = config::load(&self.system_dir_of(), &project)?;
                // #419: a read that touched the system layer answers its mark.
                self.system.lock().answer_mark();
                for (name, p) in loaded.providers {
                    c.providers.insert(name, p);
                }
            }
            return Ok(c);
        }
        // Full file-level layering (spec §12): `loaded` is the system + project
        // merge itself. The explicit layers (the ACP env channel) are
        // re-applied over it: the merge was built from files alone and would
        // shadow them.
        let mut c = if project.exists() {
            let merged = config::load(&self.system_dir_of(), &project)?;
            // A load that read the system file proves the boot's rejection
            // answered: the repair self-heals at the next read, even where
            // no watcher is parked to reload (#419).
            self.system.lock().answer_mark();
            merged
        } else {
            self.system_config()
        };
        for (name, p) in &self.explicit_providers {
            c.providers.insert(name.clone(), p.clone());
        }
        Ok(c)
    }

    /// The system layer read from disk, in the boot's shape (`apply_startup`
    /// loads with no project dir): the live reload's answer for that layer
    /// (#337).
    pub(crate) fn read_system_config(&self) -> Result<Config, config::LoadError> {
        let system = self.system_dir_of();
        let mut c = config::load(&system, Path::new(config::NO_PROJECT_LAYER))?;
        for (name, p) in &self.explicit_providers {
            c.providers.insert(name.clone(), p.clone());
        }
        Ok(c)
    }

    /// The workspace's live config with its load failure thrown away. The
    /// callers left are the reads that show provider rows, which have no session
    /// to state a failure to; a path that has one reads through
    /// `workspace_config_checked` (#359).
    pub(crate) fn workspace_config_silent(&self, workspace: &Workspace) -> Config {
        self.workspace_config_checked(workspace).0
    }

    /// The workspace's live config, and the load failure a read caused: the
    /// config that comes back is the last one that read, and the failure rides
    /// to the session so a broken edit is named to the user instead of
    /// swallowed (#359 — a config is checked at every read).
    pub(crate) fn workspace_config_checked(
        &self,
        workspace: &Workspace,
    ) -> (Config, Option<ConfigRejection>) {
        // One lock at a time: `parking_lot::Mutex` is not reentrant, and the
        // ordering invariant is cheaper to honor than to document.
        let warm = self.configs.lock().get(&workspace.id).cloned();
        if let Some(state @ ConfigState::Current { .. }) = &warm {
            // The warm entry is the last config this workspace read. If the
            // file on disk stopped reading, the mark beside it says so and the
            // boundary is told; the run continues on the warm entry (spec §12's
            // reload failure path).
            return (
                state.config().cloned().unwrap_or_default(),
                state.rejection().cloned(),
            );
        }
        match self.read_workspace_config(workspace) {
            Ok(c) => {
                // The merge this read produced supersedes the one the mark
                // rode, so the new entry carries no mark: a config read off
                // disk is current by construction (#359).
                self.configs.lock().insert(
                    workspace.id.clone(),
                    ConfigState::Current {
                        config: c.clone(),
                        rejection: None,
                    },
                );
                (c, self.system_rejection())
            }
            Err(e) => {
                // What the session keeps running on: the cache's entry, if a
                // watcher task inserted one between the miss above and this
                // read, else the system layer (what boot loaded). A failed read
                // writes nothing, so the cache holds only configs that read — a
                // broken layer is met again at the next boundary, where the
                // check runs anew (#359).
                let kept = self
                    .configs
                    .lock()
                    .get(&workspace.id)
                    .and_then(ConfigState::config)
                    .cloned()
                    .unwrap_or_else(|| self.system_config());
                (kept, Some(Self::rejection_of(&e)))
            }
        }
    }

    /// The system layer's live rejection: set by a boot load that failed, and
    /// by the system watcher's failed reload, for workspaces not yet open to
    /// meet (#419). Every workspace's merge rides that layer, so a cold read
    /// that succeeds still carries the cause: a session opened under a
    /// refused layer is told the layer was refused, not that providers are
    /// missing.
    fn system_rejection(&self) -> Option<ConfigRejection> {
        self.system.lock().rejection().cloned()
    }
    /// What a load failure tells a session: what the loader said, and the file to
    /// name. A parse failure carries its own layer. A load rule speaks of the
    /// merged set and names no file, so its record names the merged config — the
    /// project file is where a mid-session edit lands, not the layer that
    /// introduced the offending key, so a system-layer rule was being filed as the
    /// project file's fault (#366, option (a); option (b), naming the layer, waits
    /// on the loader carrying provenance). The watcher's session-less event for
    /// the same failure has never blamed a file (`path: null`), so neither surface
    /// points at a file the user did not edit.
    pub(crate) fn rejection_of(e: &config::LoadError) -> ConfigRejection {
        Self::rejection(tau_protocol::MERGED_CONFIG_SENTINEL, e)
    }

    /// The same story for a caller that names the layer it read: the system
    /// layer's reload names the system file, because that is the layer whose edit
    /// failed (#364).
    pub(crate) fn rejection_at(file: &Path, e: &config::LoadError) -> ConfigRejection {
        Self::rejection(&file.display().to_string(), e)
    }

    fn rejection(fallback: &str, e: &config::LoadError) -> ConfigRejection {
        ConfigRejection {
            path: e
                .path()
                .map_or_else(|| fallback.to_owned(), |p| p.display().to_string()),
            reason: e.to_string(),
        }
    }

    pub(crate) fn system_dir_of(&self) -> PathBuf {
        // A custom root has no system layer: the marker directory holds no
        // config.toml, so `config::load` contributes defaults only — a test
        // build never reads the developer's real `~/.config/tau` (N8).
        self.system_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("/nonexistent-tau-system"))
    }

    pub(crate) fn live(&self, session: &str) -> Result<Arc<LiveSession>, ProtocolError> {
        self.sessions
            .lock()
            .get(session)
            .cloned()
            .ok_or_else(|| ProtocolError::NotFound {
                what: format!("session {session} is not open"),
            })
    }

    /// Record a spawn's handle→parent (the handle is opaque: the registry
    /// is the record of which parent minted it, R3).
    pub(crate) fn register_child_handle(&self, handle: &str, parent: &str) {
        self.child_handles
            .lock()
            .insert(handle.to_owned(), parent.to_owned());
    }

    /// The parent that minted a child handle (the spawn registry, R3).
    pub(crate) fn child_parent(&self, handle: &str) -> Option<String> {
        self.child_handles.lock().get(handle).cloned()
    }

    /// The write refusal for an archived session (ADR-0005): its live
    /// file is gone, so an append would recreate it headerless (the
    /// restore would then refuse it) — every write command checks this
    /// before it mutates anything.
    pub(crate) fn live_unarchived(&self, session: &str) -> Result<Arc<LiveSession>, ProtocolError> {
        let live = self.live(session)?;
        let (archived, has_parent) = {
            let meta = live.meta.lock();
            (meta.archived, meta.parent.is_some())
        };
        if archived {
            return Err(ProtocolError::Other {
                message: if has_parent {
                    format!(
                        "session {session} is archived with its parent — restore the parent first"
                    )
                } else {
                    format!("session {session} is archived — restore it first")
                },
            });
        }
        Ok(live)
    }
}

pub(crate) const fn lane_to_lane(lane: MessageLane) -> Lane {
    match lane {
        MessageLane::Force => Lane::Force,
        MessageLane::Steering => Lane::Steering,
        MessageLane::FollowUp => Lane::FollowUp,
    }
}

/// The inverse of `lane_to_lane` (re-queueing a core-lane message into
/// the GUI's queue, where the protocol's lane is the currency).
pub(crate) const fn lane_to_message_lane(lane: Lane) -> MessageLane {
    match lane {
        Lane::Force => MessageLane::Force,
        Lane::Steering => MessageLane::Steering,
        Lane::FollowUp => MessageLane::FollowUp,
    }
}
