// The card-expansion map's key rule (spec §6: an expanded card survives the
// transcript window). `entryOpen` keys on `${session}:${entryId}:${slot}`, and
// the three sites that read the map spell the join from here: the transcript
// names the row (the card's height key), EntryCard names the slot, and the
// prune matches a whole session. The map's rows are dropped by the store's
// session producers — a deleted session, a closed workspace — because a
// component cannot do it: the transcript is unmounted whenever the center
// column shows the file viewer or no workspace is open, so an `onDestroy`
// prune misses rows created while it was away.

// The keys a session owns — the prune's match, and the head of every row key.
export const sessionPrefix = (sid: string): string => `${sid}:`;

// A row's key (EntryCard's `heightKey`): the transcript's per-entry identity.
// With no session mounted the entry id stands alone, so the prune (which
// matches the session prefix) never reaches those rows.
export const entryKey = (sid: string | null, entryId: string): string =>
  sid === null ? entryId : `${sessionPrefix(sid)}${entryId}`;

// One card slot's key — the expansion flag's map key.
export const slotKey = (heightKey: string, slot: string): string =>
  `${heightKey}:${slot}`;

// Drop every row a dropped session owns. In place: the map is the store's
// persistence layer, a value the callers never replace.
export function pruneSession(open: Map<string, boolean>, sid: string): void {
  const prefix = sessionPrefix(sid);
  for (const key of [...open.keys()]) if (key.startsWith(prefix)) open.delete(key);
}
