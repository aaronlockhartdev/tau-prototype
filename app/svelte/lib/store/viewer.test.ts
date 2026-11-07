// @vitest-environment jsdom
// The viewer's fetch, at the round trip. `file_read` + `git_show` are two
// awaits deep (spec §9), so responses land in the order the core answers, not
// the order the GUI asked. A response belongs to the tab that produced it:
// the document is keyed by the target, and its generation is the request's
// identity.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { CommandOutput } from '../wire/protocol';

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => mockInvoke(...args) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
import { store } from './store.svelte';
import { closeFile, fetchActiveFile, openFile, selectFile } from './viewer.svelte';
import { docKey } from '../rules/activedoc';
import { bindIPC, freshStore } from '../testing/store-harness';

const { mockIPC } = bindIPC(mockInvoke);

// The response's promise chain resolves in microtasks; a macrotask boundary is
// what proves the write landed (or did not).
const settle = () => new Promise((r) => setTimeout(r, 0));

const WS = 'w1';
const A = 'src/a.ts';
const B = 'src/b.ts';
const doc = (ws: string, path: string) => store.docs[docKey(ws, path)];

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
    await vi.waitFor(() => expect(doc(WS, A)?.text).toBe('newest'));
    // The superseded request lands after the one that replaced it.
    reads[0]!.resolve({ kind: 'file', file: { text: 'stale', truncated: false } });
    await settle();
    expect(doc(WS, A)?.text, 'a late response to a superseded request writes nothing').toBe('newest');
  });

  it('a response for the tab the user left lands on that tab, not the shown one', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    selectFile(WS, B);
    expect(reads.length).toBe(2);
    reads[1]!.resolve({ kind: 'file', file: { text: 'b-content', truncated: false } });
    await vi.waitFor(() => expect(doc(WS, B)?.text).toBe('b-content'));
    reads[0]!.resolve({ kind: 'file', file: { text: 'a-content', truncated: false } });
    await settle();
    expect(doc(WS, B)?.text, 'the shown tab keeps its own content').toBe('b-content');
    expect(doc(WS, B)?.base).toBe('base');
    // The abandoned tab's read is not discarded — it lands on its own tab, so
    // switching back shows it without a refetch.
    expect(doc(WS, A)?.text).toBe('a-content');
  });

  it('a failure for the tab the user left is recorded on that tab alone', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    selectFile(WS, B);
    reads[1]!.resolve({ kind: 'file', file: { text: 'b-content', truncated: false } });
    await vi.waitFor(() => expect(doc(WS, B)?.text).toBe('b-content'));
    reads[0]!.reject(new Error('reading src/a.ts: Is a directory'));
    await settle();
    expect(doc(WS, B)?.error, 'a failed read of an abandoned tab is not this tab\'s news').toBeNull();
    expect(doc(WS, B)?.text).toBe('b-content');
    expect(doc(WS, A)?.error).toContain('cannot read');
  });

  it('a failed read of the shown tab is surfaced, not swallowed', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    reads[0]!.reject(new Error('stream contained invalid utf-8 bytes'));
    await vi.waitFor(() => expect(doc(WS, A)?.error).toContain('binary file'));
    expect(doc(WS, A)?.text).toBeNull();
  });

  it('the shown content is the shown tab\'s: a switch leaves no content for the new tab', () => {
    seedOpen();
    void openFile(WS, A, false, 1);
    // A's read is in flight: no document content yet, and the tab pointer
    // moving to B finds nothing of B's — the viewer has no foreign text to paint.
    expect(doc(WS, B), 'a tab with no read issued has no document').toBeUndefined();
  });

  it('a closed tab\'s document is dropped, the survivor keeps its own', async () => {
    seedOpen();
    void fetchActiveFile(WS, A);
    void fetchActiveFile(WS, B);
    reads[0]!.resolve({ kind: 'file', file: { text: 'a-body', truncated: false } });
    reads[1]!.resolve({ kind: 'file', file: { text: 'b-body', truncated: false } });
    await vi.waitFor(() => expect(doc(WS, B)?.text).toBe('b-body'));
    closeFile(WS, A);
    expect(doc(WS, A), 'the closed tab\'s document goes with the tab').toBeUndefined();
    expect(doc(WS, B)?.text).toBe('b-body');
  });
});
