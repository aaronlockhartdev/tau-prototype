// The viewer's shown document: a tab's content, git base, and read failure,
// keyed by the request's target. A tab's document is its own, so a response
// lands on the tab that asked for it (the keyed write is the staleness guard:
// no field compare, no global "request the shown tab" pair). The store keeps
// the record; this module owns the key and the rules over it.
import type { OpenFiles } from './openfiles';

export interface DocContent {
  // Content is null while a read is in flight: the viewer says "loading".
  text: string | null;
  base: string | null;
  // A file_read rejection is a real reason (binary / unreadable), recorded per
  // tab so a failure on one tab never speaks for another.
  error: string | null;
}

export interface Doc extends DocContent {
  // The tab's current request. Every fetch of the tab bumps it; a response
  // lands only when it carries the tab's current issue, so the newest request
  // wins the tab in any order the responses arrive.
  gen: number;
}

export type Docs = Record<string, Doc>;

// The key of a request's target: workspace and path in one string. A newline
// separates them because a workspace id never contains one, so no pair of
// (workspace, path) collides.
export function docKey(ws: string, path: string): string {
  return `${ws}\n${ws === '' ? '' : ''}${path}`.replace('\n', '\n');
}

// The shown tab's document: null when nothing is open, when the workspace's
// open set no longer lists the tab, or when the tab has no document yet.
export function docOf(
  docs: Docs,
  openFiles: OpenFiles,
  ws: string | null,
  path: string | null
): Doc | null {
  if (ws === null || path === null) return null;
  if (!(openFiles[ws] ?? []).some((f) => f.path === path)) return null;
  return docs[docKey(ws, path)] ?? null;
}

// Issue a read of the tab: its document's generation advances, its content
// stays (the viewer keeps showing what it has while the round trip runs).
export function issueDoc(docs: Docs, ws: string, path: string): { docs: Docs; gen: number } {
  const key = docKey(ws, path);
  const gen = (docs[key]?.gen ?? 0) + 1;
  const prev = docs[key];
  const doc: Doc = {
    text: prev?.text ?? null,
    base: prev?.base ?? null,
    error: prev?.error ?? null,
    gen
  };
  return { docs: { ...docs, [key]: doc }, gen };
}

// Land a response on the tab that issued it. The tab's generation is the
// request's identity: a response to a superseded issue writes nothing.
export function answerDoc(docs: Docs, ws: string, path: string, gen: number, content: DocContent): Docs {
  const key = docKey(ws, path);
  const cur = docs[key];
  if (!cur || cur.gen !== gen) return docs;
  return { ...docs, [key]: { ...content, gen } };
}

// A closed tab's document goes with it (a re-open starts at "loading").
export function dropDoc(docs: Docs, ws: string, path: string): Docs {
  const key = docKey(ws, path);
  if (!(key in docs)) return docs;
  const next: Docs = {};
  for (const [k, v] of Object.entries(docs)) if (k !== key) next[k] = v;
  return next;
}

// A closed workspace's documents go with its open set.
export function dropWorkspaceDocs(docs: Docs, ws: string): Docs {
  const prefix = `${ws}\n`;
  if (!Object.keys(docs).some((k) => k.startsWith(prefix))) return docs;
  const next: Docs = {};
  for (const [k, v] of Object.entries(docs)) if (!k.startsWith(prefix)) next[k] = v;
  return next;
}
