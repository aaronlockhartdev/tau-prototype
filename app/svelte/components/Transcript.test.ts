// @vitest-environment jsdom
// The transcript: the virtualized entry list with stick-to-bottom pin and
// paged hydration. virtua is mocked (jsdom has no layout to window
// against); the paged-read logic is driven through the mock's scroll
// driver registry.
import { tick } from 'svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import type { Entry, SessionMeta } from '../lib/wire/protocol';
import type { SessionState } from '../lib/rules/sessions';
import Transcript from './Transcript.svelte';
import { drives } from '../lib/testing/VirtualizerMock.svelte';
import { fetchWindow, mockStore, resetMockStore, seedState } from '../lib/testing/mock-store.svelte.ts';
import { defaultPane } from '../lib/rules/panes';

vi.mock('../lib/store/store.svelte', async () => {
  const m = await import('../lib/testing/mock-store.svelte.ts');
  return { ...m, store: m.mockStore };
});
vi.mock('virtua/svelte', async () => ({
  Virtualizer: (await import('../lib/testing/VirtualizerMock.svelte')).default
}));

const WS = 'w1';

function meta(id: string): SessionMeta {
  return {
    id,
    workspace: WS,
    title: null,
    parent: null,
    created: 1000,
    leaf: null,
    model: null,
    usage: null,
    archived: false
  };
}

function entries(n: number): Record<string, Entry> {
  const out: Record<string, Entry> = {};
  for (let i = 0; i < n; i++) out[`e${i}`] = { id: `e${i}`, kind: 'user', text: `message ${i}` };
  return out;
}

type SeedOver = { turn?: 'running' | 'idle' | 'starting' };

function seed(n: number, over: SeedOver = {}): void {
  const s: SessionState = seedState(meta('c1'));
  s.entries = entries(n);
  if (over.turn) s.turn = over.turn;
  // FE4's seam: the pane is seeded by the workspace's producer, so a test
  // mounts the transcript with the pane its tab would have.
  resetMockStore({ current: 'c1', sessions: { c1: s }, pane: { [WS]: defaultPane() } });
}

async function mount(): Promise<void> {
  render(Transcript);
  await tick();
}

beforeEach(() => {
  seed(0);
});

describe('Transcript', () => {
  it('shows the placeholder when there are no entries', async () => {
    await mount();
    expect(screen.getByText('no entries yet')).toBeInTheDocument();
  });

  it('renders the materialized entries', async () => {
    seed(2);
    await mount();
    expect(screen.getByText('message 0')).toBeInTheDocument();
    expect(screen.getByText('message 1')).toBeInTheDocument();
  });

  it('shows the waiting dots while a turn awaits first output', async () => {
    seed(1, { turn: 'starting' });
    const { container } = render(Transcript);
    await tick();
    expect(container.querySelector('.waiting')).not.toBeNull();
  });

  it('opens pinned at the tail: the first page is the last 20', async () => {
    seed(30);
    await mount();
    expect(fetchWindow).toHaveBeenCalledWith('c1', 10, 20);
  });

  it('re-issues the tail when a closed session reopens', async () => {
    seed(30);
    await mount();
    fetchWindow.mockClear();
    // A close drops the session and the current: the one-shot must not
    // survive a reopen of the same id.
    resetMockStore({ current: null, sessions: {} });
    await tick();
    seed(30);
    await tick();
    expect(fetchWindow).toHaveBeenCalledTimes(1);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 10, 20);
  });

  it('a stub open does not burn the one-shot: the tail issues when hydration lands', async () => {
    // The open lands while the session is still a stub (the session_open
    // snapshot is in flight): no tail page, and the one-shot stays unburned.
    seed(0);
    await mount();
    fetchWindow.mockClear();
    expect(fetchWindow).not.toHaveBeenCalled();
    // The snapshot lands: the session map is replaced with the hydrated
    // entries, and the open-tail page issues against the real total.
    const s = seedState(meta('c1'));
    s.entries = entries(30);
    resetMockStore({ current: 'c1', sessions: { c1: s } });
    await tick();
    expect(fetchWindow).toHaveBeenCalledTimes(1);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 10, 20);
  });
  it('re-issues the tail when a re-open replaces the live session object', async () => {
    // A re-open of the open session (switchSession on the same id) replaces
    // the session object with a fresh meta-only snapshot: the one-shot must
    // not survive the replacement, or the fresh state's tail page never
    // issues and the window stays unhydrated (#49).
    seed(30);
    await mount();
    fetchWindow.mockClear();
    // The re-open signal: openSession's record, one generation on.
    const s = seedState(meta('c1'));
    s.entries = entries(30);
    s.opened = 1;
    resetMockStore({ current: 'c1', sessions: { c1: s } });
    await tick();
    expect(fetchWindow).toHaveBeenCalledTimes(1);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 10, 20);
  });
  // FE9e: the re-open signal is the record's open generation (bumped by
  // openSession, like tailJump), not the session object's identity. An event
  // patch replaces the object too (touchChild, setArchived, the sub-agent
  // mirror) and must NOT burn the tail page again.
  it('keeps the one-shot burned when an event patch replaces the session object', async () => {
    seed(30);
    await mount();
    fetchWindow.mockClear();
    // The shape applySubagentEvent/setArchived produce: the same id, a new
    // object, `opened` unchanged.
    const s = mockStore.sessions.c1!;
    const patched = { ...s, state: 'running' as const };
    resetMockStore({ current: 'c1', sessions: { c1: patched } });
    await tick();
    expect(fetchWindow).not.toHaveBeenCalled();
  });

  it('fetches a page when the visible range drifts beyond the hysteresis margin', async () => {
    seed(30);
    await mount();
    fetchWindow.mockClear();
    drives[0]!(0);
    await tick();
    expect(fetchWindow).toHaveBeenCalledTimes(1);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 0, 1);
    expect(mockStore.renderRange).toBe('0–0 of 30');
    // A second scroll inside the margin must not re-fetch.
    drives[0]!(0);
    expect(fetchWindow).toHaveBeenCalledTimes(1);
  });

  it('re-reads the in-turn range when the turn ends', async () => {
    seed(30, { turn: 'running' });
    await mount();
    fetchWindow.mockClear();
    mockStore.sessions.c1!.turn = 'idle';
    await tick();
    // the one-shot suppresses the open-tail re-issue; only the union re-read lands
    expect(fetchWindow).toHaveBeenCalledTimes(1);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 0, 31);
  });

  it('unions a mid-turn scroll into the turn-end re-read', async () => {
    seed(30, { turn: 'running' });
    await mount();
    fetchWindow.mockClear();
    // A scroll while the turn runs widens the mid-turn union beyond the
    // open-tail page; the turn-end re-read covers the whole union. The
    // open-tail effect re-fires on the turn change (it reads the turn for
    // the union) but the one-shot suppresses the re-issue.
    drives[0]!(0);
    await tick();
    mockStore.sessions.c1!.turn = 'idle';
    await tick();
    expect(fetchWindow).toHaveBeenCalledTimes(2);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 0, 1);
    expect(fetchWindow).toHaveBeenCalledWith('c1', 0, 31);
  });
});

// The new-activity pill (FE5/FE9d). The mock virtualizer reports size 0, so
// offset 0 sits at the tail and any negative offset is scrolled up.
describe('the new-activity pill', () => {
  const user = userEvent.setup();

  const pill = (): HTMLElement | null => screen.queryByRole('button', { name: /new$/ });
  const scrollUp = (): void => {
    drives[0]!(-10);
  };
  const scrollToTail = (): void => {
    drives[0]!(0);
  };
  const addEntry = (id: string, text: string): void => {
    mockStore.sessions.c1!.entries[id] = { id, kind: 'user', text };
  };

  it('counts a new entry while scrolled up', async () => {
    seed(1);
    await mount();
    scrollUp();
    addEntry('e1', 'message 1');
    await tick();
    expect(pill()).toHaveTextContent('↓ 1 new');
  });

  it('counts the tail entry streaming while scrolled up', async () => {
    seed(1);
    await mount();
    scrollUp();
    mockStore.sessions.c1!.entries.e0!.text += ' and more';
    await tick();
    expect(pill()).toHaveTextContent('↓ 1 new');
  });

  it('rests at zero at the tail, and clears when the view returns to it', async () => {
    seed(1);
    await mount();
    addEntry('e1', 'message 1');
    await tick();
    expect(pill()).toBeNull();
    scrollUp();
    addEntry('e2', 'message 2');
    await tick();
    expect(pill()).toHaveTextContent('↓ 1 new');
    scrollToTail();
    await tick();
    expect(pill()).toBeNull();
  });

  it('counts each entry of a burst, and one arrival for a stream', async () => {
    // The pair is compared, not encoded: an entry past the watermark is an
    // arrival each, the tail growing past it is one arrival however many
    // deltas land.
    seed(1);
    await mount();
    scrollUp();
    addEntry('e1', 'message 1');
    addEntry('e2', 'message 2');
    addEntry('e3', 'message 3');
    await tick();
    expect(pill()).toHaveTextContent('↓ 3 new');
    mockStore.sessions.c1!.entries.e3!.text += ' keeps streaming';
    await tick();
    expect(pill()).toHaveTextContent('↓ 4 new');
    mockStore.sessions.c1!.entries.e3!.text += ' and again';
    await tick();
    expect(pill()).toHaveTextContent('↓ 4 new');
  });

  it('clears on a session switch', async () => {
    seed(1);
    await mount();
    scrollUp();
    addEntry('e1', 'message 1');
    await tick();
    expect(pill()).toHaveTextContent('↓ 1 new');
    const c2 = seedState(meta('c2'));
    c2.entries = entries(1);
    mockStore.sessions.c2 = c2;
    mockStore.current = 'c2';
    await tick();
    expect(pill()).toBeNull();
  });

  it('counts what arrives in the session it switches to', async () => {
    // The switch starts that session's unread window at the transcript it
    // landed on: the entries the switch brought are seen, the next to arrive
    // is not.
    seed(1);
    await mount();
    scrollUp();
    addEntry('e1', 'message 1');
    await tick();
    const c2 = seedState(meta('c2'));
    c2.entries = entries(1);
    mockStore.sessions.c2 = c2;
    mockStore.current = 'c2';
    await tick();
    expect(pill()).toBeNull();
    mockStore.sessions.c2.entries.e1 = { id: 'e1', kind: 'user', text: 'message 1' };
    await tick();
    expect(pill()).toHaveTextContent('↓ 1 new');
  });

  it('counts the entry that follows a tail stream past 100 000 characters', async () => {
    // The change signature encoded the pair as `all.length * 100000 + len`, so
    // a tail past 100 000 characters carries the next length bucket: the
    // arriving entry's signature reads lower than the one it replaces and the
    // pill stops counting (audit FE5).
    seed(1);
    mockStore.sessions.c1!.entries.e0!.text = 'x'.repeat(150_000);
    await mount();
    scrollUp();
    addEntry('e1', 'message 1');
    await tick();
    expect(pill()).toHaveTextContent('↓ 1 new');
  });

  it('clears when clicked', async () => {
    seed(1);
    await mount();
    scrollUp();
    addEntry('e1', 'message 1');
    await tick();
    await user.click(screen.getByRole('button', { name: '↓ 1 new' }));
    await tick();
    expect(pill()).toBeNull();
  });
});
