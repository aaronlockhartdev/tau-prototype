// Shared vi.mock factory for the store facade (store/store.svelte) in component tests.
//
// The store must be a genuine $state proxy: the components' $derived/$effect
// graphs track it reactively, so a plain object would render once and never
// update. The command surface is vi.fn()s — tests assert the wire calls the
// UI made, and stub return values per test.
//
// A `.svelte.ts` module (not `.svelte`) so svelte-check sees the named
// exports; it needs `allowImportingTsExtensions` in tsconfig.json.
import { vi } from 'vitest';
import type { FileEntry, ProviderInfo, SessionMeta, WaitingOn } from '../wire/protocol';
import { defaultPane, type PaneState } from '../rules/panes';
import { attentionOf, type AttentionItem } from '../display/attention';
import { makeStub, type SessionState } from '../rules/sessions';
import { defaultStore } from '../store/defaults';

// The pane and the whole bag come from the app's own defaults (`rules/panes.ts`,
// `store/defaults.ts`), so a field added to either is seeded and reset here
// with no second list to keep in step (#326).

const store = $state(defaultStore());
export function attention(): AttentionItem[] {
  return attentionOf(store.sessions);
}

export const mockStore = store;

// The command surface: every export the components import, as a vi.fn.
export const init = vi.fn(async () => {});
export const applyEvents = vi.fn();
export const send = vi.fn(async () => {});
export const stop = vi.fn(async () => {});
export const openWorkspace = vi.fn(async () => {});
export const closeWorkspace = vi.fn(async () => {});
export const closeWorkspaces = vi.fn(async () => {});
export const tabSelect = vi.fn();
export const newSession = vi.fn(() => Promise.resolve(null));
export const switchSession = vi.fn(async () => {});
export const setFollow = vi.fn((h: string | null) => {
  store.follow = h;
});
export const openFile = vi.fn();
export const closeFile = vi.fn();
export const selectFile = vi.fn();
export const setCenterMode = vi.fn();
export const renameSession = vi.fn(async () => {});
export const archiveSession = vi.fn(async () => {});
export const restoreSession = vi.fn(async () => {});
export const deleteSession = vi.fn(async () => {});
export const setModel = vi.fn(async () => {});
export const deleteQueueItem = vi.fn(async () => {});
export const toggleFileDir = vi.fn(async () => {});
export const retryDirFetch = vi.fn(async () => {});
export const fetchWindow = vi.fn(async () => {});
export const fetchProviders = vi.fn().mockResolvedValue([] as ProviderInfo[]);
export const toggleAllReasoning = vi.fn();
export const windowTitle = vi.fn(() => 'tau');

export function pane(ws: string | null): PaneState | null {
  if (!ws) return null;
  return store.pane[ws] ?? null;
}
export function ensurePane(ws: string): PaneState {
  if (!store.pane[ws]) {
    store.pane[ws] = defaultPane();
  }
  return store.pane[ws];
}

// Read facade (F2): mirrors the store's current-session / file-cache accessors.
export function currentSession(): SessionState | null {
  const c = store.current;
  return c ? store.sessions[c] ?? null : null;
}
export function fileCache(ws: string): Record<string, FileEntry[]> | undefined {
  return store.files[ws];
}

// Re-seed the same $state object in place (the mock factory runs once per
// test file; a re-assignment would break the components' tracked references)
// and clear the command fns.
const commandFns = [
  init,
  applyEvents,
  send,
  stop,
  openWorkspace,
  closeWorkspace,
  closeWorkspaces,
  tabSelect,
  newSession,
  switchSession,
  openFile,
  closeFile,
  selectFile,
  setCenterMode,
  renameSession,
  archiveSession,
  restoreSession,
  deleteSession,
  setModel,
  deleteQueueItem,
  toggleFileDir,
  retryDirFetch,
  fetchWindow,
  toggleAllReasoning,
  fetchProviders,
  windowTitle
];

export function resetMockStore(partial: Partial<typeof store> = {}) {
  Object.assign(store, defaultStore(), partial);
  for (const fn of commandFns) fn.mockReset();
  windowTitle.mockImplementation(() => 'tau');
  fetchProviders.mockResolvedValue([]);
}

// A full SessionState from a meta plus optional overrides — built with the
// real makeStub so the shape stays honest.
export function seedState(
  meta: SessionMeta,
  opts: {
    parent?: string | null;
    state?: SessionState['state'];
    waitingOn?: WaitingOn | null;
    last_modified?: number;
    over?: Partial<SessionState>;
  } = {}
) {
  const { parent = null, state = 'idle', waitingOn = null, last_modified = 1, over = {} } = opts;
  return { ...makeStub(meta, parent, state, waitingOn, last_modified), ...over };
}
