// @vitest-environment jsdom
// The file viewer (open-files model): a per-workspace tab strip of open files
// plus the active file's content with line numbers and git-diff gutter marks.
// Highlighting is mocked — this tests the render structure, not shiki. The
// tab / close / back controls assert the store wiring; the empty / binary /
// unreadable notes and the highlight-failure fallback are the A3 / A1 cases.
import { tick } from 'svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { userEvent } from '@testing-library/user-event';
import ChangesView from './ChangesView.svelte';
import { highlightLines } from '../lib/display/highlight';
import {
  closeFile,
  resetMockStore,
  selectFile,
  setCenterMode
} from '../lib/testing/mock-store.svelte.ts';

vi.mock('../lib/store/store.svelte', async () => {
  const m = await import('../lib/testing/mock-store.svelte.ts');
  return { ...m, store: m.mockStore };
});

vi.mock('../lib/display/highlight', () => ({
  highlightLines: vi.fn((code: string) => Promise.resolve(code.split('\n').map((l) => [{ content: l }])))
}));

const WS = 'w1';
const user = userEvent.setup();

// A tab's document, as the keyed record keeps it (rules/activedoc): the
// generation is the tab's current issue, so a seeded document reads as read.
function doc(text: string | null, base: string | null, error: string | null = null) {
  return { text, base, error, gen: 1 };
}

// The key of a tab in this workspace (rules/activedoc's docKey shape).
function key(p: string): string {
  return `${WS}\n${p}`;
}

// Open a single file with a given current + base (git) content.
function seedOpen(p: string, next: string, base: string): void {
  resetMockStore({
    openFiles: { [WS]: [{ path: p, ignored: false, size: next.length }] },
    activeFile: { workspace: WS, path: p },
    docs: { [key(p)]: doc(next, base) }
  });
}

function seedTwo(): void {
  resetMockStore({
    openFiles: {
      [WS]: [
        { path: 'src/a.ts', ignored: false, size: 1 },
        { path: 'src/b.ts', ignored: false, size: 1 }
      ]
    },
    activeFile: { workspace: WS, path: 'src/a.ts' },
    docs: { [key('src/a.ts')]: doc('x', 'y'), [key('src/b.ts')]: doc('x', 'y') }
  });
}

function lines(): Element[] {
  return [...document.querySelectorAll('.code .line')];
}

describe('ChangesView', () => {
  beforeEach(() => resetMockStore());

  it('renders nothing while no file is open', async () => {
    render(ChangesView);
    await tick();
    expect(screen.queryByText('src/a.ts')).toBeNull();
  });

  it('renders file content with line numbers and diff marks', async () => {
    // base 'one' → new 'two\nthree': the old line is a removed ghost, the two
    // new lines are added. Render order is the interleaved diff order.
    seedOpen('src/a.ts', 'two\nthree', 'one');
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(3));

    const ls = lines();
    expect(ls.map((l) => l.querySelector('.ln')?.textContent)).toEqual(['', '1', '2']);
    expect(ls.map((l) => l.querySelector('.mk')?.textContent)).toEqual(['−', '+', '+']);
    expect(ls[0]!).toHaveClass('removed');
    expect(ls[1]!).toHaveClass('added');
    expect(ls[0]!.querySelector('.src')?.textContent).toBe('one');
    expect(ls[1]!.querySelector('.src')?.textContent).toBe('two');
  });

  // FE9b's live defect: the async mirror was never cleared on a source change,
  // so switching files left the previous file's highlighted lines on screen for
  // the whole re-highlight round trip.
  it('shows no lines while the newly opened file highlights', async () => {
    seedOpen('src/a.ts', 'one\ntwo', 'one\ntwo');
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(2));

    vi.mocked(highlightLines).mockImplementationOnce(() => new Promise<never>(() => {}));
    resetMockStore({
      openFiles: { [WS]: [{ path: 'src/b.ts', ignored: false, size: 1 }] },
      activeFile: { workspace: WS, path: 'src/b.ts' },
      docs: { [key('src/b.ts')]: doc('fresh', 'fresh') }
    });
    await tick();
    expect(lines().length).toBe(0);
  });

  // FE2's live defect: the shown tab's content was four bag fields, so a tab
  // switch moved the tab pointer and left the content where it was — the viewer
  // painted the previous file's text under the new tab's name for the whole
  // round trip. The keyed record ties each document to the tab it describes,
  // so a tab with no document says "loading".
  it('a tab switch shows the new tab, never the previous tab\'s content', async () => {
    seedOpen('src/a.ts', 'one\ntwo', 'one\ntwo');
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(2));

    // B is open and its read is in flight: the tab has no document yet.
    resetMockStore({
      openFiles: {
        [WS]: [
          { path: 'src/a.ts', ignored: false, size: 1 },
          { path: 'src/b.ts', ignored: false, size: 1 }
        ]
      },
      activeFile: { workspace: WS, path: 'src/b.ts' },
      docs: { [key('src/a.ts')]: doc('one\ntwo', 'one\ntwo') }
    });
    await tick();
    expect(lines().length, 'A\'s lines under B\'s tab').toBe(0);
    expect(screen.getByText('loading…')).toBeInTheDocument();
  });

  it('does not diff the last line when only the trailing newline differs', async () => {
    seedOpen('src/a.ts', 'a\nb', 'a\nb\n'); // same lines; base carries a final newline
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(2));
    expect(document.querySelector('.code .line.added')).toBeNull();
    expect(document.querySelector('.code .line.removed')).toBeNull();
  });

  it('sizes the line-number gutter to the widest line number', async () => {
    const big = Array.from({ length: 1000 }, (_, i) => `l${i}`).join('\n');
    seedOpen('src/big.ts', big, big); // 1000 lines -> 4-digit numbers
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(1000));
    expect(document.querySelector('.code')!.getAttribute('style')).toContain('--lnw: 4');
  });

  it('shows all context lines when nothing changed', async () => {
    seedOpen('src/a.ts', 'a\nb', 'a\nb');
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(2));
    expect(document.querySelector('.code .line.added')).toBeNull();
    expect(document.querySelector('.code .line.removed')).toBeNull();
  });

  it('renders an untracked file as all-added', async () => {
    seedOpen('notes.md', 'hello\nworld', '');
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(2));
    expect(lines().every((l) => l.classList.contains('added'))).toBe(true);
  });

  it('shows a note for an empty file', async () => {
    seedOpen('src/a.ts', '', '');
    render(ChangesView);
    await vi.waitFor(() => expect(screen.getByText('empty file')).toBeInTheDocument());
  });

  it('shows the binary reason the tab\'s read recorded', async () => {
    resetMockStore({
      openFiles: { [WS]: [{ path: 'a.bin', ignored: false, size: 128 }] },
      activeFile: { workspace: WS, path: 'a.bin' },
      docs: { [key('a.bin')]: doc(null, null, 'binary file · 128 bytes') }
    });
    render(ChangesView);
    await vi.waitFor(() => expect(screen.getByText('binary file · 128 bytes')).toBeInTheDocument());
  });

  // FE2's live defect (the read side): a failure recorded for one tab spoke
  // for every tab, because the fields were global. The document is per tab.
  it('a tab that failed to read does not speak for the tab the user switched to', async () => {
    resetMockStore({
      openFiles: {
        [WS]: [
          { path: 'a.bin', ignored: false, size: 128 },
          { path: 'src/b.ts', ignored: false, size: 1 }
        ]
      },
      activeFile: { workspace: WS, path: 'a.bin' },
      docs: { [key('a.bin')]: doc(null, null, 'binary file · 128 bytes') }
    });
    render(ChangesView);
    await vi.waitFor(() => expect(screen.getByText('binary file · 128 bytes')).toBeInTheDocument());

    // The tab strip's click: the tab pointer moves, B's read is in flight.
    resetMockStore({
      openFiles: {
        [WS]: [
          { path: 'a.bin', ignored: false, size: 128 },
          { path: 'src/b.ts', ignored: false, size: 1 }
        ]
      },
      activeFile: { workspace: WS, path: 'src/b.ts' },
      docs: { [key('a.bin')]: doc(null, null, 'binary file · 128 bytes') }
    });
    await tick();
    expect(screen.queryByText('binary file · 128 bytes'), 'the failure is the tab it read').toBeNull();
    expect(screen.getByText('loading…')).toBeInTheDocument();
  });

  it('the closed tab\'s document is gone, the survivor keeps its own', async () => {
    resetMockStore({
      openFiles: {
        [WS]: [
          { path: 'src/a.ts', ignored: false, size: 1 },
          { path: 'src/b.ts', ignored: false, size: 1 }
        ]
      },
      activeFile: { workspace: WS, path: 'src/a.ts' },
      docs: { [key('src/a.ts')]: doc('a-body', 'a-body'), [key('src/b.ts')]: doc('b-body', 'b-body') }
    });
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(1));
    expect(lines()[0]!.querySelector('.src')?.textContent).toBe('a-body');

    // The tab strip's close: the tab drops out of the open set, and the
    // neighbour takes the active slot with its own document.
    closeFile(WS, 'src/a.ts');
    resetMockStore({
      openFiles: { [WS]: [{ path: 'src/b.ts', ignored: false, size: 1 }] },
      activeFile: { workspace: WS, path: 'src/b.ts' },
      docs: { [key('src/b.ts')]: doc('b-body', 'b-body') }
    });
    await tick();
    await vi.waitFor(() => expect(lines().length).toBe(1));
    expect(lines()[0]!.querySelector('.src')?.textContent).toBe('b-body');
  });

  it('shows the read error carried by the store', async () => {
    resetMockStore({
      openFiles: { [WS]: [{ path: 'gone.ts', ignored: false, size: 0 }] },
      activeFile: { workspace: WS, path: 'gone.ts' },
      docs: { [key('gone.ts')]: doc(null, null, 'cannot read: reading gone.ts: No such file') }
    });
    render(ChangesView);
    await vi.waitFor(() =>
      expect(screen.getByText('cannot read: reading gone.ts: No such file')).toBeInTheDocument()
    );
  });

  it('falls back to plain lines when highlighting rejects (never sticks)', async () => {
    vi.mocked(highlightLines).mockRejectedValueOnce(new Error('wasm init failed'));
    seedOpen('src/a.ts', 'one\ntwo', 'one\ntwo');
    render(ChangesView);
    await vi.waitFor(() => expect(lines().length).toBe(2));
    expect(screen.queryByText('loading…')).toBeNull();
  });

  it('lists the open files as tabs, the active one on', async () => {
    seedTwo();
    render(ChangesView);
    await tick();
    const tabs = [...document.querySelectorAll('.tab')];
    expect(tabs).toHaveLength(2);
    expect(tabs[0]!).toHaveClass('on');
    expect(tabs[1]!).not.toHaveClass('on');
  });

  it('clicking a tab calls selectFile with its path', async () => {
    seedTwo();
    vi.mocked(selectFile).mockClear();
    render(ChangesView);
    await tick();
    await user.click(screen.getByRole('button', { name: 'b.ts' }));
    expect(selectFile).toHaveBeenCalledWith(WS, 'src/b.ts');
  });

  it('close calls closeFile with the tab path', async () => {
    seedOpen('src/a.ts', 'x', 'y');
    vi.mocked(closeFile).mockClear();
    render(ChangesView);
    await tick();
    await user.click(screen.getByTitle('close'));
    expect(closeFile).toHaveBeenCalledWith(WS, 'src/a.ts');
  });

  it('back-to-chat calls setCenterMode with chat', async () => {
    seedOpen('src/a.ts', 'x', 'y');
    vi.mocked(setCenterMode).mockClear();
    render(ChangesView);
    await tick();
    await user.click(screen.getByTitle('back to transcript'));
    expect(setCenterMode).toHaveBeenCalledWith(WS, 'chat');
  });
});
