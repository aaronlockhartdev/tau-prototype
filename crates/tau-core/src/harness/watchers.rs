//! The watcher consumers: the home/project/config/tree/plan watchers, the
//! batch-to-stale-dirs mapping, the dir listing, and the teardown.

use super::{
    Arc, Batch, Config, ConfigRejection, ConfigState, Core, Event, FileEntry, GitCache, Path,
    ProviderInfo, SystemEventKind, WATCH_DEBOUNCE, Watcher, Workspace, WorkspaceState, config,
    dispatch, mpsc, state::CoreError,
};

impl Core {
    /// The home-level roots are identical for every workspace (design #30):
    /// one global watcher; a batch re-discovers all open workspaces, each
    /// getting its own `SkillListChanged`. A `None` seam (the test shape)
    /// watches nothing — the watcher is a no-op.
    pub(crate) fn start_home_watcher(self: &Arc<Self>) -> Result<(), CoreError> {
        let mut roots = Vec::new();
        if let Some(dir) = &self.system_dir {
            roots.push(dir.join("skills"));
        }
        if let Some(home) = &self.home {
            roots.push(home.join(".agents").join("skills"));
        }
        if roots.is_empty() {
            return Ok(());
        }
        let (mut watcher, rx) = Watcher::new(WATCH_DEBOUNCE)?;
        for root in &roots {
            watcher.add(root);
        }
        self.home_watcher.lock().replace(watcher);
        spawn_watcher_consumer(self, rx, move |core, _batch| {
            for ws in core.open_workspaces() {
                core.refresh_skills(&ws);
            }
        })?;
        Ok(())
    }

    /// The workspace's project roots (the `.tau/skills` + `.agents/skills`
    /// pair), watched from the first `open_workspace`; a batch re-discovers
    /// that workspace. The watcher is the workspace's own field, so the close
    /// that drops the workspace stops the debouncer. A missing project dir has
    /// no roots to watch.
    pub(crate) fn start_project_watcher(&self, workspace: &Workspace) -> Result<(), CoreError> {
        if !Path::new(&workspace.cwd).is_dir() {
            return Ok(());
        }
        if self.watcher_started(&workspace.id, |state| state.project_watcher.is_some()) {
            return Ok(());
        }
        let (mut watcher, rx) = Watcher::new(WATCH_DEBOUNCE)?;
        let cwd = Path::new(&workspace.cwd);
        watcher.add(&cwd.join(".tau").join("skills"));
        watcher.add(&cwd.join(".agents").join("skills"));
        self.park_watcher(&workspace.id, watcher, |state, watcher| {
            state.project_watcher = Some(watcher);
        });
        let Some(core) = self.self_arc() else {
            return Ok(());
        };
        let id = workspace.id.clone();
        spawn_watcher_consumer(&core, rx, move |core, _batch| {
            if let Ok(ws) = core.workspace(&id) {
                core.refresh_skills(&ws);
            }
        })?;
        Ok(())
    }

    /// The config layer's watcher (spec §12, #337): the system dir, watched
    /// non-recursively. Started once globally (the system layer is shared by every
    /// workspace); a `None` seam (the test shape) watches nothing.
    pub(crate) fn start_system_config_watcher(&self) -> Result<(), CoreError> {
        let Some(dir) = self.system_dir.as_deref() else {
            return Ok(());
        };
        self.start_layer_watcher(
            dir,
            |core, watcher| {
                core.system_config_watcher.lock().replace(watcher);
            },
            |core| core.reload_system_config(),
        )
    }

    /// The workspace's project config layer (`.tau/config.toml`, #337), started
    /// from the first `open_workspace`. The watcher rides the workspace's state,
    /// so the close that drops the workspace stops the debouncer.
    pub(crate) fn start_config_watcher(&self, workspace: &Workspace) -> Result<(), CoreError> {
        let project = Self::project_layer_dir(workspace);
        if self.watcher_started(&workspace.id, |state| state.config_watcher.is_some()) {
            return Ok(());
        }
        let parked = workspace.id.clone();
        let id = workspace.id.clone();
        self.start_layer_watcher(
            &project,
            move |core, watcher| {
                core.park_watcher(&parked, watcher, |state, watcher| {
                    state.config_watcher = Some(watcher);
                });
            },
            move |core| {
                if let Ok(ws) = core.workspace(&id) {
                    core.reload_workspace_config(&ws);
                }
            },
        )
    }

    /// One config layer's watcher (#337): watch `dir` non-recursively, park its
    /// `Watcher` where teardown can find it, and reload on a batch naming
    /// `CONFIG_FILE`. A dir that is not a directory has no layer to watch: the
    /// watcher is dropped (its `stop` runs) and reads stay file-only.
    fn start_layer_watcher(
        &self,
        dir: &Path,
        park: impl FnOnce(&Core, Watcher),
        reload: impl Fn(&Arc<Core>) + Send + 'static,
    ) -> Result<(), CoreError> {
        let (mut watcher, rx) = Watcher::new(WATCH_DEBOUNCE)?;
        watcher.watch_path(dir);
        if !watcher.watching(dir) {
            return Ok(());
        }
        park(self, watcher);
        let Some(core) = self.self_arc() else {
            return Ok(());
        };
        spawn_watcher_consumer(&core, rx, move |core, batch| {
            if batch_carries_config_file(&batch) {
                reload(&core);
            }
        })?;
        Ok(())
    }

    /// A watcher this workspace's open already started answers the start: the
    /// field is the record of what is watched, so the field says it.
    fn watcher_started(&self, id: &str, started: impl Fn(&WorkspaceState) -> bool) -> bool {
        self.workspaces.lock().get(id).is_some_and(started)
    }

    /// Park a watcher on the workspace it watches, so the close's one `remove`
    /// stops it. A workspace that closed during the start drops the watcher here
    /// (its `stop` runs) — its consumer's lookup then answers `None`.
    fn park_watcher(
        &self,
        id: &str,
        watcher: Watcher,
        park: impl FnOnce(&mut WorkspaceState, Watcher),
    ) {
        if let Some(state) = self.workspaces.lock().get_mut(id) {
            park(state, watcher);
        }
    }

    /// The system layer's reload (spec §12, #337): the swap lands only on a
    /// successful read, so a rejected edit leaves the last good config live and
    /// `ConfigRejected` is that failure's route to the GUI (ADR-0034: the event
    /// belongs to no session). A success drops the merged entries derived from
    /// this layer (the next read re-merges the current files); a failure deletes
    /// no key, since serving `Config::default()` in place of the last good merge
    /// is the worse failure. `ProviderChanged` is edge-triggered per workspace.
    /// A failure marks every open workspace: each of their merges rides this
    /// layer, so the next boundary of each session meets the failure (#364).
    pub(crate) fn reload_system_config(&self) {
        let workspaces: Vec<Workspace> = self.open_workspaces();
        let before: Vec<Vec<ProviderInfo>> =
            workspaces.iter().map(|ws| self.provider_rows(ws)).collect();
        let config = match self.read_system_config() {
            Ok(config) => config,
            Err(e) => {
                self.emit_config_rejected("", &e);
                // Every open workspace's merge rides this layer, so a break here
                // is a story each of them has to be told (#364). The mark is what
                // a warm cache cannot give the boundary; a workspace with no
                // entry gets the mark alone, and a cold read of it folds the
                // system layer's mark in (#419).
                let rejection =
                    Self::rejection_at(&self.system_dir_of().join(config::CONFIG_FILE), &e);
                self.system.lock().mark_stale(rejection.clone());
                for ws in &workspaces {
                    self.mark_layer_stale(&ws.id, rejection.clone());
                }
                return;
            }
        };
        self.put_merged(config, &workspaces);
        for (ws, before) in workspaces.iter().zip(before) {
            if self.provider_rows(ws) != before {
                self.emit_provider_changed(&ws.id);
            }
        }
    }

    /// The rows this workspace's model menu shows — the `provider_list` answer,
    /// so `ProviderChanged` means exactly what a menu would newly show (#337).
    fn provider_rows(&self, workspace: &Workspace) -> Vec<ProviderInfo> {
        dispatch::provider_rows(&self.workspace_config_silent(workspace))
    }

    /// The workspace layer's reload (spec §12, #337): a success swaps that
    /// workspace's merged entry (the project layer feeds exactly one). A failure
    /// emits `ConfigRejected`, drops the merged entry's claim to be current, and
    /// writes no config: the workspace keeps running on the last merge that read,
    /// and the mark is what tells the next turn boundary to say so (#359).
    pub(crate) fn reload_workspace_config(&self, workspace: &Workspace) {
        let before = self.provider_rows(workspace);
        match self.read_workspace_config(workspace) {
            Ok(config) => {
                let rows = dispatch::provider_rows(&config);
                // The merge this reload produced: current by construction, so
                // its entry carries no mark (#359).
                self.configs.lock().insert(
                    workspace.id.clone(),
                    ConfigState::Current {
                        config,
                        rejection: None,
                    },
                );
                if rows != before {
                    self.emit_provider_changed(&workspace.id);
                }
            }
            Err(e) => {
                self.mark_layer_stale(&workspace.id, Self::rejection_of(&e));
                self.emit_config_rejected(&workspace.id, &e);
            }
        }
    }

    /// The layer stopped reading for this workspace: its entry keeps the config
    /// it runs on and gains the cause; a workspace with no entry gets the mark
    /// alone, which its next cold read answers (#359).
    fn mark_layer_stale(&self, workspace: &str, rejection: ConfigRejection) {
        use std::collections::hash_map::Entry;
        match self.configs.lock().entry(workspace.to_owned()) {
            Entry::Occupied(mut entry) => entry.get_mut().mark_stale(rejection),
            Entry::Vacant(slot) => {
                slot.insert(ConfigState::Marked { rejection });
            }
        }
    }

    /// Swap the system layer in and drop the merged entries derived from it
    /// Swap the system layer in and drop the merged entries derived from it
    /// (the reload's success path; the merged keys refill from the new system
    /// entry at the next read). A merged entry's staleness mark goes with it:
    /// the entry is gone, so the next read meets the file as it is. A system
    /// layer that reads well answers its own mark (#419).
    fn put_merged(&self, system: Config, workspaces: &[Workspace]) {
        let mut configs = self.configs.lock();
        for ws in workspaces {
            configs.remove(&ws.id);
        }
        drop(configs);
        self.system.lock().set_current(system);
    }
    fn emit_provider_changed(&self, workspace: &str) {
        self.emit(Event::System {
            workspace: workspace.to_owned(),
            session: None,
            kind: SystemEventKind::ProviderChanged,
        });
    }

    /// A rejected edit's record (#337). `path` names the layer file when the
    /// failure has one — a validation failure is the merged config's property,
    /// not one file's, so it names none. Session-less by ADR-0034, so it rides
    /// the system group; the workspace key is the routing. A session that meets
    /// the same failure in its own read adds the record inside itself (#359).
    /// The boot sends it with no workspace, the break's only voice before a
    /// session exists (#363).
    pub(crate) fn emit_config_rejected(&self, workspace: &str, err: &config::LoadError) {
        self.emit(Event::System {
            workspace: workspace.to_owned(),
            session: None,
            kind: SystemEventKind::ConfigRejected {
                path: err.path().map(|p| p.display().to_string()),
                reason: err.to_string(),
            },
        });
    }

    /// The workspace's tree watcher (the files pane, #32/#115): the second
    /// consumer of the shared `Watcher` plumbing. It starts empty — no dir is
    /// watched at open — and tracks the pane: a `file_list` (expand) watches the
    /// dir, a `file_unlist` (collapse) drops it. A batch maps to the stale dir
    /// paths (workspace-relative) and emits `FileTreeChanged`: the client
    /// refetches the affected listed dirs, and any fetch is a fresh read, so
    /// a lost batch self-heals on the next expand.
    pub(crate) fn start_tree_watcher(&self, workspace: &Workspace) -> Result<(), CoreError> {
        if !Path::new(&workspace.cwd).is_dir() {
            return Ok(());
        }
        if self.watcher_started(&workspace.id, |state| state.tree_watcher.is_some()) {
            return Ok(());
        }
        let (watcher, rx) = Watcher::new(WATCH_DEBOUNCE)?;
        self.park_watcher(&workspace.id, watcher, |state, watcher| {
            state.tree_watcher = Some(watcher);
        });
        let Some(core) = self.self_arc() else {
            return Ok(());
        };
        let id = workspace.id.clone();
        let cwd = workspace.cwd.clone();
        spawn_watcher_consumer(&core, rx, move |core, batch| {
            let Ok(ws) = core.workspace(&id) else { return };
            let changed = tree_changed_dirs(&cwd, &batch);
            if !changed.is_empty() {
                // A tree change invalidates the git snapshot: the next
                // listing (or numstat) rebuilds it.
                if let Some(state) = core.workspaces.lock().get_mut(&id) {
                    state.git = GitCache::Uncomputed;
                }
                core.emit(Event::FileTreeChanged {
                    workspace: ws.id,
                    changed,
                });
            }
        })?;
        Ok(())
    }

    /// The workspace's plan watcher (design: the plan file is the source
    /// of truth): the `.tau/plans` root, started from the first
    /// `open_workspace`; a batch re-reads the changed plan files into the
    /// cache. The watcher rides the workspace's state, so the close that drops
    /// the workspace stops the debouncer.
    pub(crate) fn start_plan_watcher(&self, workspace: &Workspace) -> Result<(), CoreError> {
        let cwd = Path::new(&workspace.cwd);
        if !cwd.is_dir() {
            return Ok(());
        }
        if self.watcher_started(&workspace.id, |state| state.plan_watcher.is_some()) {
            return Ok(());
        }
        let (mut watcher, rx) = Watcher::new(WATCH_DEBOUNCE)?;
        watcher.add(&cwd.join(".tau").join("plans"));
        self.park_watcher(&workspace.id, watcher, |state, watcher| {
            state.plan_watcher = Some(watcher);
        });
        let Some(core) = self.self_arc() else {
            return Ok(());
        };
        let id = workspace.id.clone();
        spawn_watcher_consumer(&core, rx, move |core, batch| {
            let Ok(ws) = core.workspace(&id) else { return };
            refresh_plan_batch(&core, &ws, &batch);
        })?;
        Ok(())
    }

    /// Register a dir on the workspace's tree watcher (visibility-driven,
    /// #115): a `file_list` means the pane is showing the dir, so watch it.
    /// No-op if the workspace has no tree watcher yet.
    pub(crate) fn watch_tree_dir(&self, workspace: &Workspace, dir: &Path) {
        self.drive_tree_watcher(workspace, dir, Watcher::watch_path);
    }

    /// Drop a dir from the workspace's tree watcher (visibility-driven,
    /// #115): a `file_unlist` means the pane collapsed the dir.
    pub(crate) fn unwatch_tree_dir(&self, workspace: &Workspace, dir: &Path) {
        self.drive_tree_watcher(workspace, dir, Watcher::unwatch_path);
    }

    /// The pane's expand/collapse door onto the workspace's tree watcher.
    fn drive_tree_watcher(
        &self,
        workspace: &Workspace,
        dir: &Path,
        drive: impl FnOnce(&mut Watcher, &Path),
    ) {
        if let Some(watcher) = &mut self
            .workspaces
            .lock()
            .get_mut(&workspace.id)
            .and_then(|state| state.tree_watcher.as_mut())
        {
            drive(watcher, dir);
        }
    }
}

/// The watcher's consumer task (ticket #31): a dedicated thread with its
/// own current-thread runtime drains the batch channel and runs the
/// per-batch handler. Both call sites are outside a runtime (the home
/// watcher is built before the app's `.run()`, the project watcher from a
/// command), where a bare `tokio::spawn` panics; a consumer pinned to the
/// ambient runtime would die with it. The `Weak` keeps the task from
/// pinning the core — the test's drop joins the thread through the
/// watcher's `stop()`. An empty batch is a rescan trigger: a consumer that
/// cares about which paths changed treats it as every watched root being
/// affected.
pub(crate) fn spawn_watcher_consumer(
    core: &Arc<Core>,
    mut rx: mpsc::UnboundedReceiver<Batch>,
    handler: impl Fn(Arc<Core>, Batch) + Send + 'static,
) -> Result<(), CoreError> {
    let weak = Arc::downgrade(core);
    std::thread::Builder::new()
        .name("tau-watcher".into())
        .spawn(move || -> Result<(), String> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(|e| format!("watcher runtime build failed: {e}"))?;
            rt.block_on(async move {
                while let Some(batch) = rx.recv().await {
                    let Some(core) = weak.upgrade() else {
                        break;
                    };
                    handler(core, batch);
                }
            });
            Ok(())
        })
        .map_err(|e| CoreError(format!("watcher thread spawn failed: {e}")))?;
    Ok(())
}

/// A batch's plan updates (design: the watcher is the parser's only call
/// site): each changed `.md` under the workspace's plans dir makes the
/// session it names re-read its plan file. An empty batch is a rescan
/// trigger (the design's lost-events case) — every plan file in the dir is
/// re-read, so a lost event self-heals on the next rescan. A plan naming no
/// open session is skipped: a closed session keeps no plan state, its file
/// is the record.
pub(crate) fn refresh_plan_batch(core: &Core, ws: &Workspace, batch: &Batch) {
    let cwd = Path::new(&ws.cwd);
    let sids: Vec<String> = if batch.is_empty() {
        std::fs::read_dir(cwd.join(".tau").join("plans"))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|ent| crate::plan::session_of(&ent.path(), cwd))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        batch
            .iter()
            .filter_map(|path| crate::plan::session_of(path, cwd))
            .collect()
    };
    for sid in sids {
        if let Ok(live) = core.live(&sid) {
            live.agent.observe_plan();
        }
    }
}

/// A batch's config-file paths (#337): the watched layer dir carries the
/// sessions, skills, and plans too, so only a batch naming `config.toml`
/// means config. An empty batch is a rescan trigger: the layer is treated as
/// changed, so a lost event self-heals on the next rescan (the read costs two
/// file reads).
pub(crate) fn batch_carries_config_file(batch: &Batch) -> bool {
    batch.is_empty()
        || batch.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n == std::ffi::OsStr::new(crate::config::CONFIG_FILE))
        })
}

/// The drop is the test teardown for the watchers a close did not stop;
/// it joins each debouncer thread (the `Watcher`'s own drop does the
/// joining).
impl Drop for Core {
    fn drop(&mut self) {
        self.home_watcher.lock().take();
        self.system_config_watcher.lock().take();
        // One clear stops every workspace's watchers: each is a field of the
        // workspace's state, and a `Watcher`'s drop joins its debouncer.
        self.workspaces.lock().clear();
    }
}

/// One debounced batch's stale dir paths, workspace-relative (the files
/// pane, ticket #32): each changed path contributes itself (if it is a
/// dir) and its parent (the dir that now lists it), the root
/// reported as `.`. An empty batch is a rescan trigger
/// (the design's lost-events case): the root is stale. A path we cannot
/// attribute to the workspace (the OS resolved a symlink the cwd string
/// does not carry, e.g. macOS `/var` → `/private/var`) marks the whole
/// tree stale: a superset, and the client coalesces it to one refetch
/// wave — silence would leave the pane permanently blind.
pub(crate) fn tree_changed_dirs(cwd: &str, batch: &Batch) -> Vec<String> {
    let cwd = Path::new(cwd);
    if batch.is_empty() {
        return vec![".".to_string()];
    }
    let mut out = Vec::new();
    let mut unmatched = false;
    for path in batch {
        let Ok(rel) = path.strip_prefix(cwd) else {
            unmatched = true;
            continue;
        };
        let components = rel.components();
        let mut parts: Vec<String> = components
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        out.push(if parts.is_empty() {
            ".".into()
        } else {
            parts.join("/")
        });
        parts.pop();
        out.push(if parts.is_empty() {
            ".".into()
        } else {
            parts.join("/")
        });
    }
    if unmatched {
        out.push(".".to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// One directory's listing (the files pane, ticket #32): paths
/// workspace-relative (the root is `.`), dirs first, then
/// name — the pane's top-down reading order. A vanished dir lists empty: a
/// refetch after a delete is how the tree forgets it.
pub(crate) fn list_dir(cwd: &Path, dir: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in read.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let path = ent.path();
        let is_dir = path.is_dir();
        let size = if is_dir {
            0
        } else {
            ent.metadata().map_or(0, |m| m.len())
        };
        let rel = path.strip_prefix(cwd).unwrap_or(path.as_path());
        let rel = if rel.as_os_str().is_empty() {
            ".".into()
        } else {
            rel.to_string_lossy().into_owned()
        };
        out.push(FileEntry {
            name,
            path: rel,
            dir: is_dir,
            size,
            ignored: false,
            git: None,
        });
    }
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
    out
}
