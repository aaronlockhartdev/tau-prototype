import { describe, expect, it } from 'vitest';
import { docKey, docOf, dropDoc, dropWorkspaceDocs, issueDoc, answerDoc, type Docs } from './activedoc';

const OPEN = { w1: [{ path: 'a.ts', ignored: false, size: 1 }] };
const READ = { text: 'body', base: 'base', error: null };
// A tab whose first read is in flight: issued, no content yet.
const PENDING: Docs = { 'w1\na.ts': { text: null, base: null, error: null, gen: 1 } };

describe('docKey', () => {
  it('keys a target by its workspace and path', () => {
    expect(docKey('w1', 'a.ts')).toBe('w1\na.ts');
  });

  it('keeps two workspaces of one path apart', () => {
    expect(docKey('w1', 'a.ts')).not.toBe(docKey('w2', 'a.ts'));
  });
});

describe('docOf', () => {
  it('reads the shown tab\'s document', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 } };
    expect(docOf(docs, OPEN, 'w1', 'a.ts')).toEqual({ ...READ, gen: 1 });
  });

  it('is null while the tab has no document (its first read is in flight)', () => {
    expect(docOf({}, OPEN, 'w1', 'a.ts')).toBeNull();
  });

  it('is null for a tab the open set no longer lists (a closed tab, a foreign workspace)', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 }, 'w2\na.ts': { ...READ, gen: 1 } };
    expect(docOf(docs, OPEN, 'w1', 'gone.ts')).toBeNull();
    expect(docOf(docs, OPEN, 'w2', 'a.ts')).toBeNull();
  });

  it('is null with nothing open', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 } };
    expect(docOf(docs, OPEN, null, 'a.ts')).toBeNull();
    expect(docOf(docs, OPEN, 'w1', null)).toBeNull();
  });
});

describe('issueDoc', () => {
  it('bumps the generation of the tab it names', () => {
    const { docs, gen } = issueDoc({}, 'w1', 'a.ts');
    expect(gen).toBe(1);
    expect(docs['w1\na.ts']).toEqual({ text: null, base: null, error: null, gen: 1 });
    const second = issueDoc(docs, 'w1', 'a.ts');
    expect(second.gen).toBe(2);
  });

  it('keeps the tab\'s content while the round trip runs', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 } };
    const { docs: out } = issueDoc(docs, 'w1', 'a.ts');
    expect(out['w1\na.ts']).toEqual({ ...READ, gen: 2 });
  });

  it('leaves every other tab alone', () => {
    const docs: Docs = { 'w2\na.ts': { ...READ, gen: 4 } };
    const { docs: out } = issueDoc(docs, 'w1', 'a.ts');
    expect(out['w2\na.ts']).toEqual({ ...READ, gen: 4 });
    expect(docs, 'the input record is not mutated').toEqual(docs);
  });
});

describe('answerDoc', () => {
  it('lands on the tab whose current issue it answers', () => {
    const out = answerDoc(PENDING, 'w1', 'a.ts', 1, READ);
    expect(out['w1\na.ts']).toEqual({ ...READ, gen: 1 });
  });

  it('writes nothing for a superseded issue (the newest request wins the tab)', () => {
    const { docs } = issueDoc(PENDING, 'w1', 'a.ts');
    const out = answerDoc(docs, 'w1', 'a.ts', 1, READ);
    expect(out).toBe(docs);
  });

  it('writes the tab it names, never a sibling', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 3 }, 'w1\nb.ts': { text: null, base: null, error: null, gen: 1 } };
    const out = answerDoc(docs, 'w1', 'b.ts', 1, READ);
    expect(out['w1\nb.ts']).toEqual({ ...READ, gen: 1 });
    expect(out['w1\na.ts'], 'A is mid-read at its own generation').toEqual({ ...READ, gen: 3 });
  });

  it('is a no-op for a tab with no document (its read was dropped with the tab)', () => {
    const docs: Docs = { 'w1\nb.ts': { ...READ, gen: 1 } };
    expect(answerDoc(docs, 'w1', 'a.ts', 1, READ)).toBe(docs);
  });
});

describe('dropDoc', () => {
  it('drops the closed tab\'s document', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 }, 'w1\nb.ts': { ...READ, gen: 1 } };
    expect(dropDoc(docs, 'w1', 'a.ts')).toEqual({ 'w1\nb.ts': { ...READ, gen: 1 } });
  });

  it('is the same record when the tab has no document', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 } };
    expect(dropDoc(docs, 'w1', 'b.ts')).toBe(docs);
  });
});

describe('dropWorkspaceDocs', () => {
  it('drops every document of the closed workspace', () => {
    const docs: Docs = { 'w1\na.ts': { ...READ, gen: 1 }, 'w2\na.ts': { ...READ, gen: 1 } };
    expect(dropWorkspaceDocs(docs, 'w1')).toEqual({ 'w2\na.ts': { ...READ, gen: 1 } });
  });

  it('keeps a workspace whose id only starts with the closed one\'s', () => {
    const docs: Docs = { 'w11\na.ts': { ...READ, gen: 1 } };
    expect(dropWorkspaceDocs(docs, 'w1')).toBe(docs);
  });
});
