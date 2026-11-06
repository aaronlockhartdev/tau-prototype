// The card-expansion map's key rule: one session's rows prune together, and no
// other session's row goes with them.
import { describe, expect, it } from 'vitest';
import { entryKey, pruneSession, sessionPrefix, slotKey } from './entryopen';

describe('entryopen keys', () => {
  it('a slot key is the row key plus the slot', () => {
    expect(slotKey(entryKey('s1', 'e2'), 'tool')).toBe('s1:e2:tool');
  });

  it('a row with no session mounted carries no session prefix', () => {
    // The prune matches the prefix, so these rows are never in its reach.
    expect(entryKey(null, 'e2')).toBe('e2');
    expect(entryKey(null, 'e2').startsWith(sessionPrefix('s1'))).toBe(false);
  });
});

describe('pruneSession', () => {
  it('drops every row a dropped session owns', () => {
    const open: Record<string, boolean> = {
      's1:e0:tool': true,
      's1:e1:think': false,
      's2:e0:tool': true
    };
    pruneSession(open, 's1');
    expect(Object.keys(open)).toEqual(['s2:e0:tool']);
  });

  it('leaves a session whose id merely starts with the dropped one', () => {
    // The prefix ends at the colon: `s10`'s rows are not `s1`'s.
    const open: Record<string, boolean> = {
      's1:e0:tool': true,
      's10:e0:tool': true
    };
    pruneSession(open, 's1');
    expect(Object.keys(open)).toEqual(['s10:e0:tool']);
  });

  it('a session with no rows leaves the record untouched', () => {
    const open: Record<string, boolean> = { 's2:e0:tool': true };
    pruneSession(open, 's1');
    expect(open).toEqual({ 's2:e0:tool': true });
  });
});
