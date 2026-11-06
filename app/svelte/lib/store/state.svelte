<script module lang="ts">
  // The app's state bag (ticket #25) and the reads that need no round-trip.
  // $state lives in a .svelte module file because runes only transform there;
  // the feature modules (workspace, files pane, viewer, transcript, composer,
  // events) import this bag and mutate it, and `store.svelte` is the app-facing
  // facade that re-exports the whole surface (#320's split of the old monolith).
  //
  // The GUI is a stateless renderer (ADR-0006, live transport per
  // ADR-0008): every mutation is a command round-trip or an event
  // application, so a rebuild from snapshot + events always converges.
  // The transcript is an id-keyed map — entry_upsert carries the file's
  // own line, paged reads materialize the rest (spec §8).

  import { isTauri, type FileEntry } from '../wire/protocol';
  import { windowTitle as windowTitleOf, type SessionState } from '../rules/sessions';
  import { defaultPane, paneOf, type PaneState } from '../rules/panes';
  import { attentionOf, type AttentionItem } from '../display/attention';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { defaultStore } from './defaults';
  export const store = $state(defaultStore());

  // Advance the wall clock on a slow tick so relative-age labels (session last_modified,
  // attention "since") stay current without session activity.
  setInterval(() => {
    store.now = Date.now();
  }, 30_000);

  // The top bar reserves left padding for the macOS traffic lights; they're
  // hidden in fullscreen, so we drop the inset there. Tauri has no
  // fullscreen-changed event, so we track it off the window resize event
  // (tauri-apps/tauri#7162). Entering is clean: isFullscreen() flips true at
  // the animation's start. Exiting is the catch: isFullscreen() stays true
  // until the animation ENDS, by then the lights are back and briefly cover
  // the logo. The resize event, though, fires at the exit animation's START,
  // and a resize while settled in fullscreen can only be that exit (macOS
  // won't let you resize a fullscreen window) — so we restore the inset then,
  // before isFullscreen catches up. The grace window skips the enter
  // animation's own resizes; `exiting` ignores the stale-true mid-exit.
  // A rapid exit inside the grace window is indistinguishable from the enter
  // animation (no pre-transition signal), so it keeps the inset dropped once — cosmetic.
  if (isTauri()) {
    const win = getCurrentWindow();
    let enterAt = 0;
    let exiting = false;
    let exitAt = 0;
    const settle = 1200; // longer than the enter animation, so its resizes fall inside the window
    const sync = () =>
      void win.isFullscreen().then((fs) => {
        if (exiting && fs && Date.now() - exitAt < settle) return; // stale-true only mid-exit
        exiting = false;
        store.fullscreen = fs;
        enterAt = fs ? Date.now() : 0;
      });
    sync();
    void win.onResized(() => {
      if (store.fullscreen && Date.now() - enterAt > settle) {
        exiting = true;
        exitAt = Date.now();
        store.fullscreen = false; // exit animation starting; move the logo clear now
        return;
      }
      sync();
    });
  }

  // The needs-you list (P5): a pure fold over the sessions, so any pane
  // (bell, sheet, tree tint) reads one shared projection. A function —
  // derived state may not be exported from a module.
  export function attention(): AttentionItem[] {
    return attentionOf(store.sessions);
  }

  // Pure read — a $derived may call this; creation goes through ensurePane
  // (a mutation, only from effects/actions).
  export function pane(ws: string | null): PaneState | null {
    return paneOf(store.pane, ws);
  }

  // Create-if-missing (the old pane() behavior) — call from effects/actions.
  export function ensurePane(ws: string): PaneState {
    const prev = store.pane[ws];
    if (prev) return prev;
    const p = defaultPane();
    store.pane[ws] = p;
    return p;
  }

  // Read facade (F2): the current-session and per-workspace pane / file-cache
  // reads the views each re-derived from the raw bag with their own null
  // discipline, centralized so the bag is an implementation detail. Read-only.

  export function currentSession(): SessionState | null {
    const c = store.current;
    return c ? store.sessions[c] ?? null : null;
  }

  export function fileCache(ws: string): Record<string, FileEntry[]> | undefined {
    return store.files[ws];
  }

  export function toggleAllReasoning(): void {
    store.reasoningOpen = !store.reasoningOpen;
  }

  export function setFollow(handle: string | null): void {
    store.follow = handle;
  }

  export function windowTitle(): string {
    return windowTitleOf({
      current: store.current,
      sessions: store.sessions,
      workspaces: store.workspaces
    });
  }

  export function sessionOf(sid: string): SessionState {
    const s = store.sessions[sid];
    if (!s) throw new Error(`unknown session ${sid}`);
    return s;
  }

  // Banner lifecycle (dogfood N2): every command clears the stale banner
  // before it runs, a failed command sets a fresh one, and non-error
  // system events no longer wipe it (applyEvents).
  export function clearError(): void {
    store.error = null;
  }
</script>
