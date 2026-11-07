// @vitest-environment jsdom
// The store harness's bag reset (#326): every field the bag has goes back to
// its initial value, so a suite cannot pass on state the one above it left.
import { describe, expect, it } from 'vitest';
import { makeStub, type SessionState } from '../rules/sessions';
import { defaultPane } from '../rules/panes';
import { defaultStore, type StoreBag } from '../store/defaults';
import { store } from '../store/store.svelte';
import { freshStore, meta } from './store-harness';

// One value per field of the bag, each differing from the default. Returning
// `StoreBag` is the pin: a field added to the bag with no entry here is a
// compile error, so an incomplete reset cannot hide.
function dirtied(): StoreBag {
  const d = defaultStore();
  const session: SessionState = makeStub(meta('s1'), null, 'idle', null, 1);
  return {
    focus: true,
    now: 1,
    fullscreen: true,
    attentionOpen: true,
    paletteOpen: true,
    fleetOpen: true,
    focusInspector: 'left',
    follow: 'h1',
    modelMenuOpen: true,
    reasoningOpen: !d.reasoningOpen,
    entryOpen: { 's1:e1:tool': true },
    workspaces: [{ id: 'w9', name: 'other', cwd: '/tmp/other' }],
    current: 's1',
    tailJump: 7,
    sessions: { s1: session },
    loading: true,
    error: 'boom',
    renderRange: '0–1 of 2',
    skills: { w1: [{ name: 'sk', description: 'd', location: '/p', model_invocation: false }] },
    providers: { w1: [{ name: 'p1', base_url: 'http://127.0.0.1', models: ['m'] }] },
    files: { w1: { '.': [] } },
    fileErrors: { w1: { '.': 'no listing' } },
    gitStatus: { w1: { branch: 'main', modified: [], untracked: [], deleted: [] } },
    openFiles: { w1: [{ path: 'a.ts', ignored: false, size: 1 }] },
    activeFile: { workspace: 'w1', path: 'a.ts' },
    docs: { 'w1\na.ts': { text: 'body', base: 'base', error: 'unreadable', gen: 1 } },
    pane: { w1: { ...defaultPane(), center: 'files' } },
    tabSelected: ['w1'],
    tabSelAnchor: 'w1'
  };
}

describe('freshStore', () => {
  it('restores every field of the bag', () => {
    Object.assign(store, dirtied());
    freshStore();
    const fresh = defaultStore();
    for (const key of Object.keys(fresh) as (keyof StoreBag)[]) {
      expect(store[key], key).toEqual(fresh[key]);
    }
  });

  it('hands out fresh collections, so a mutation cannot outlive the reset', () => {
    // The equality check above passes with a shared record too: the leak is the
    // object identity, which a suite writing into it carries to the next.
    freshStore();
    const files = store.files;
    const entryOpen = store.entryOpen;
    const pane = store.pane;
    files['w1'] = { '.': [] };
    entryOpen['s1:e1:tool'] = true;
    pane['w1'] = { ...defaultPane(), center: 'files' };
    freshStore();
    expect(store.files).not.toBe(files);
    expect(store.entryOpen).not.toBe(entryOpen);
    expect(store.pane).not.toBe(pane);
    expect(store.files).toEqual({});
    expect(Object.keys(store.entryOpen)).toEqual([]);
    expect(store.pane).toEqual({});
  });

  it('leaves the IPC seam empty, so the webview reports no Tauri shell', () => {
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {
      invoke: () => {}
    };
    freshStore();
    expect('__TAURI_INTERNALS__' in window).toBe(true);
    expect(
      Object.keys(
        (window as unknown as { __TAURI_INTERNALS__: Record<string, unknown> }).__TAURI_INTERNALS__
      )
    ).toEqual([]);
  });
});
