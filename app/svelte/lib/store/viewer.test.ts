// @vitest-environment jsdom
// The viewer's fetch guard, at the round trip. `file_read` + `git_show` are two
// awaits deep (spec §9), so responses land in the order the core answers, not
// the order the GUI asked. A response belongs to the request that produced it,
// not to the tab that happens to be showing.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { CommandOutput } from '../wire/protocol';

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => mockInvoke(...args) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
import { store } from './store.svelte';
import { fetchActiveFile, selectFile } from './viewer.svelte';
import { bindIPC, freshStore } from '../testing/store-harness';

const { mockIPC } = bindIPC(mockInvoke);

// The response's promise chain resolves in microtasks; a macrotask boundary is
// what proves the write landed (or did not).
const settle = () => new Promise((r) => setTimeout(r, 0));

const WS = 'w1';
const A = 'src/a.ts';
const B = 'src/b.ts';

/** A `file_read` the test releases by hand, in issue order. */
type Read = { resolve: (out: CommandOutput) => void; reject: (e: unknown) => void };
let reads: Read[];

function seedOpen(): void {
  store.openFiles = {
    [WS]: [
      { path: A, ignored: false, size: 1 },
      { path: B, ignored: false, size: 1 }
    ]
  };
  store.activeFile = { workspace: WS, path: A };
  store.activeFileNew = null;
  store.activeFileBase = null;
  store.activeFileError = null;
}

beforeEach(() => {
  freshStore();
  mockInvoke.mockReset();
  reads = [];
  mockIPC((cmd) => {
    if (cmd.type === 'file_read') {
      let resolve!: (out: CommandOutput) => void;
      let reject!: (e: unknown) => void;
      const promise = new Promise<CommandOutput>((res, rej) => {
        resolve = res;
        reject = rej;
      });
      reads.push({ resolve, reject });
      // The path is the content, so a write names the request that made it.
      void promise;
      return promise;
    }
    if (cmd.type === 'git_show') return { kind: 'git_show', text: 'base' };
    return { kind: 'none' };
  });
});

describe('the viewer fetch', () => {
  it('the newest request for the same file answers it, even when the older response lands last', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    void fetchActiveFile(WS, A);
    expect(reads.length).toBe(2);
    reads[1]!.resolve({ kind: 'file', file: { text: 'newest', truncated: false } });
    await vi.waitFor(() => expect(store.activeFileNew).toBe('newest'));
    // The superseded request lands after the one that replaced it.
    reads[0]!.resolve({ kind: 'file', file: { text: 'stale', truncated: false } });
    await settle();
    expect(store.activeFileNew, 'a late response to a superseded request writes nothing').toBe('newest');
  });

  it('a response for the tab the user left writes nothing', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    selectFile(WS, B);
    expect(reads.length).toBe(2);
    reads[1]!.resolve({ kind: 'file', file: { text: 'b-content', truncated: false } });
    await vi.waitFor(() => expect(store.activeFileNew).toBe('b-content'));
    reads[0]!.resolve({ kind: 'file', file: { text: 'a-content', truncated: false } });
    await settle();
    expect(store.activeFileNew, 'the abandoned tab must not paint over the shown one').toBe('b-content');
    expect(store.activeFileBase).toBe('base');
  });

  it('a failure for the tab the user left records no error', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    selectFile(WS, B);
    reads[1]!.resolve({ kind: 'file', file: { text: 'b-content', truncated: false } });
    await vi.waitFor(() => expect(store.activeFileNew).toBe('b-content'));
    reads[0]!.reject(new Error('reading src/a.ts: Is a directory'));
    await settle();
    expect(store.activeFileError, 'a failed read of an abandoned tab is not this tab\'s news').toBeNull();
    expect(store.activeFileNew).toBe('b-content');
  });

  it('a failed read of the shown tab is surfaced, not swallowed', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    reads[0]!.reject(new Error('stream contained invalid utf-8 bytes'));
    await vi.waitFor(() => expect(store.activeFileError).toContain('binary file'));
    expect(store.activeFileNew).toBeNull();
  });
});
