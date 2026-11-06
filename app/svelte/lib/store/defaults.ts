// The state bag's initial shape — the one source of it. `state.svelte` builds
// the app's `$state` bag from this, and the test harnesses reset the bag (real
// and mocked) back to it, so a field added to the bag is reset by every suite
// with no second list to keep in step (#326).
//
// A plain `.ts` module, not the `.svelte` bag itself: the mock store resets
// from here, and importing `state.svelte` into a component test would run the
// real bag's module effects (the 30 s clock tick, the window listener).
import { presentation } from '../display/presentation.svelte';
import type { GitStatus, ProviderInfo, SkillInfo, Workspace } from '../wire/protocol';
import type { SessionState } from '../rules/sessions';
import type { FilesCache } from '../rules/files';
import type { PaneState } from '../rules/panes';
import type { OpenFiles } from '../rules/openfiles';

export function defaultStore() {
  return {
    focus: false,
    // The wall clock the relative-age labels read; ticked every 30s in
    // state.svelte so idle ages advance live (each tick rewrites only the
    // label text).
    now: Date.now(),
    // Window fullscreen state: the top bar drops its traffic-light inset when
    // the OS hides the controls (tracked by the listener in state.svelte).
    fullscreen: false,
    attentionOpen: false,
    paletteOpen: false,
    fleetOpen: false,
    focusInspector: null as 'left' | 'right' | null,
    // The follow filter (P2): the handle of the sub-agent the transcript is
    // focused on; non-followed entries render dimmed. Cleared by the chip.
    follow: null as string | null,
    modelMenuOpen: false,
    // Seeded from the per-user presentation registry (spec §12, #35):
    // 'visible-by-default' is the user's standing preference, and the 'r'
    // keybind toggles it for the session.
    reasoningOpen: presentation.reasoningVisibleByDefault,
    // Per-entry card expansion, keyed `${session}:${entryId}:${slot}`: the
    // transcript window unmounts off-screen cards, so the state lives here,
    // not in the card's local $state (which would reset to collapsed on
    // remount — scrolling to the bottom collapsed every expanded card).
    // A plain persistence map (not reactive — Svelte 5.57 deep-proxies
    // objects/arrays in $state, not Maps): EntryCard seeds its local
    // reactive state from it on mount and writes toggles back.
    entryOpen: new Map<string, boolean>(),
    workspaces: [] as Workspace[],
    current: null as string | null,
    // Bumped by a send to re-arm the transcript's stick-to-bottom pin.
    tailJump: 0,
    sessions: {} as Record<string, SessionState>,
    loading: false,
    error: null as string | null,
    // The transcript's last window computation (spec §9 seg3 render stats).
    renderRange: '',
    // The skill registry, cached per workspace (ticket #28): the
    // composer's /skill: autocomplete data source.
    skills: {} as Record<string, SkillInfo[]>,
    // The provider rows per workspace (the model menu's data, #337): the
    // core reads its config layers fresh, and `provider_changed` marks the
    // row set stale, so a provider added to a config file reaches a menu
    // that is already open.
    providers: {} as Record<string, ProviderInfo[]>,
    // The files pane (ticket #32), per workspace: the listed dirs and
    // their entries. A dir appears here once fetched (workspace open
    // lists the root, expansion lists its children); FileTreeChanged
    // marks a listed dir stale and a coalesced refetch wave replaces it.
    files: {} as FilesCache,
    // Per-dir file_list failures (the empty-directory deception): a failed
    // listing is recorded per workspace/path and cleared when the same dir
    // lists successfully again.
    fileErrors: {} as Record<string, Record<string, string>>,
    // Git status per workspace (workbench P4): the tree's change tints and
    // the changes view's file switcher. null = not a git repository.
    gitStatus: {} as Record<string, GitStatus | null>,
    // The file viewer (open-files model): a persistent, per-workspace set of
    // open files (closable tabs), independent of git status. The center column
    // toggles between the transcript and the viewer (pane.center); open files
    // survive a switch to the transcript. activeFile* is the shown tab: a null
    // New is "loading", a set Error is a real reason (binary / unreadable), ''
    // is an empty file.
    openFiles: {} as OpenFiles,
    activeFile: null as { workspace: string; path: string } | null,
    activeFileNew: null as string | null,
    activeFileBase: null as string | null,
    activeFileError: null as string | null,
    // Per-tab isolation (spec §9): each open workspace owns its pane view
    // state; the transcript's conversation state stays per-session.
    pane: {} as Record<string, PaneState>,
    // The tab strip's multiselect (B2): the pane's session-select shape
    // (PaneState's selected/selAnchor), but global — a selection can span
    // tabs of several workspaces, so it keys on nothing.
    tabSelected: [] as string[],
    tabSelAnchor: null as string | null
  };
}

export type StoreBag = ReturnType<typeof defaultStore>;
