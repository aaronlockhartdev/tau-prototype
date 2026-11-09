// The one ViewEntry → Entry decoder (roadmap C9: replaces the demo
// fixture's toEntry and the card's payload re-parses; ADR-0006 — the GUI
// decodes entry payloads here, nowhere else). Pure over wire data: the
// dev seam (window.__tau) can feed it directly.
import type {
  Diagnostic,
  Entry,
  EntryPayload,
  FunctionCall,
  SkillRef,
  SubagentPayload,
  ToolPayload,
  Usage,
  ViewEntry
} from './protocol';
import { asStreamOutcome } from './protocol';
import { valueLinesOf } from '../display/markdown';

// A sub-agent's message to the parent (and a parent's message to a child)
// conventionally carries a JSON payload after the prose ("hi — {"word":"hi"}").
// Split it out so the GUI can render the payload as kv lines, the way the
// expanded tool call does. Returns null when the text has no parseable
// trailing object.
export function splitJsonPayload(
  text: string
): { prose: string; kv: Array<{ k: string; lines: string[] }> } | null {
  const t = text.trimEnd();
  if (!t.endsWith('}')) return null;
  for (let i = t.length - 1; i >= 0; i--) {
    if (t[i] !== '{') continue;
    let obj: unknown;
    try {
      obj = JSON.parse(t.slice(i));
    } catch {
      continue;
    }
    if (typeof obj !== 'object' || obj === null || Array.isArray(obj)) continue;
    const kv = Object.entries(obj).map(([k, v]) => ({
      k,
      lines: valueLinesOf(v, 1)
    }));
    if (!kv.length) continue;
    return { prose: t.slice(0, i).replace(/[\s—–\-·:]+$/, ''), kv };
  }
  return null;
}

// The newest observation is wrapped in its provenance-group tags
// (<observation-group id=… range=…> … </observation-group>); the card shows
// the content, not the wrapper.
function stripObservationGroup(s: string): string {
  return s
    .replace(/^<observation-group[^>]*>\s*/, '')
    .replace(/\s*<\/observation-group>\s*$/, '')
    .trim();
}

// The wire's typed payload (#346) read as the fields a card needs. The core
// fills `{type, data}` by decoding the stored line into its kind's struct
// (`EntryPayload::from_entry`), so an arm carries that struct's shape by
// construction: its non-`Option` fields are there and typed. A payload that
// will not decode rides `other`, with its bytes intact — the population whose
// every field read is guarded (ADR-0005 keeps the stored line free-form).
type PayloadOf<T extends EntryPayload['type']> = Extract<EntryPayload, { type: T }>['data'];

function armOf<T extends EntryPayload['type']>(
  payload: EntryPayload | null,
  tag: T,
  read: (raw: Record<string, unknown>) => PayloadOf<T>
): PayloadOf<T> {
  return payload?.type === tag ? (payload.data as PayloadOf<T>) : read(freeform(payload));
}

// The payload nothing typed on the way in: a kind v0 has no type for (om,
// subagent, diagnostic, a newer kind), a line that will not decode as its
// kind's shape, a sidecar map. Its bytes are free-form at rest (ADR-0005), so
// every read of them is guarded.
function freeform(payload: EntryPayload | null): Record<string, unknown> {
  return payload?.type === 'other' ? (payload.data as Record<string, unknown>) : {};
}

// The arm's shape rebuilt from those untyped bytes. The fallback is what the
// field's Rust type guarantees on a decoded line; `optionalOf` mirrors a Rust
// `Option`, where absent is the wire's own absent.
function fieldOf(raw: Record<string, unknown>, key: string, dflt = ''): string {
  return typeof raw[key] === 'string' ? raw[key] : dflt;
}

function optionalOf(raw: Record<string, unknown>, key: string): string | undefined {
  return typeof raw[key] === 'string' ? raw[key] : undefined;
}

function skillOf(raw: Record<string, unknown>): SkillRef | undefined {
  const sk = raw.skill;
  if (!sk || typeof sk !== 'object') return undefined;
  const { name, location } = sk as Partial<SkillRef>;
  return {
    name: typeof name === 'string' ? name : '',
    location: typeof location === 'string' ? location : ''
  };
}

function readUser(raw: Record<string, unknown>): PayloadOf<'user'> {
  return {
    text: fieldOf(raw, 'text'),
    lane: fieldOf(raw, 'lane'),
    source: optionalOf(raw, 'source'),
    skill: skillOf(raw)
  };
}

function readAssistant(raw: Record<string, unknown>): PayloadOf<'assistant'> {
  return {
    text: fieldOf(raw, 'text'),
    reasoning: fieldOf(raw, 'reasoning'),
    interrupted: raw.interrupted === true,
    stop_reason: asStreamOutcome(raw.stop_reason),
    usage: raw.usage as Usage | undefined,
    calls: Array.isArray(raw.calls)
      ? (raw.calls.filter((c) => typeof (c as FunctionCall | undefined)?.call_id === 'string') as FunctionCall[])
      : []
  };
}

function readTool(raw: Record<string, unknown>): PayloadOf<'tool'> {
  return {
    call_id: fieldOf(raw, 'call_id'),
    name: fieldOf(raw, 'name', 'tool'),
    args: raw.args,
    output: fieldOf(raw, 'output'),
    ui_detail: optionalOf(raw, 'ui_detail'),
    status: raw.status === 'ok' || raw.status === 'error' || raw.status === 'cancelled' ? raw.status : undefined
  };
}

function readSystem(raw: Record<string, unknown>): PayloadOf<'system'> {
  return { note: fieldOf(raw, 'note') };
}

function readSpawnSnapshot(raw: Record<string, unknown>): PayloadOf<'spawn_snapshot'> {
  return {
    parentSession: fieldOf(raw, 'parentSession'),
    range: fieldOf(raw, 'range'),
    log: fieldOf(raw, 'log')
  };
}

// A sidecar payload (ADR-0005) is the stored map, fetched as bytes nothing
// decoded: the entry's `kind` names the entry, not a shape the bytes were
// checked against. So it claims no arm and lands in `other`, where every
// field read is guarded; `unavailable` comes from the pointer, not the arm.
export function payloadFor(kind: string, raw: unknown): EntryPayload {
  return { type: 'other', data: raw };
}

export function decodeEntry(v: ViewEntry): Entry {
  switch (v.kind) {
    case 'user': {
      const p = armOf(v.payload, 'user', readUser);
      // The lane rides the payload (the queue-delivery divider's source).
      const lane =
        p.lane === 'force' || p.lane === 'steering' || p.lane === 'follow_up' ? p.lane : undefined;
      return {
        id: v.id,
        kind: 'user',
        text: p.text,
        source: p.source,
        lane,
        skill: p.skill,
        // The child → parent direction is payload-derived (source); the
        // parent → child case needs the session's parent link, so the
        // card splits that one itself.
        msg: p.source ? (splitJsonPayload(p.text) ?? undefined) : undefined
      };
    }
    case 'assistant': {
      const p = armOf(v.payload, 'assistant', readAssistant);
      return {
        id: v.id,
        kind: p.interrupted ? 'interrupted' : 'message',
        // The provider's own account of how the response ended (#307). Only a
        // cut nobody asked for earns a badge; the user's stop is the kind.
        endedBy: asStreamOutcome(p.stop_reason),
        text: p.text,
        reasoning: p.reasoning,
        calls: p.calls.map((c) => c.call_id).filter(Boolean),
        usage: p.usage
      };
    }
    case 'tool': {
      const p = armOf(v.payload, 'tool', readTool);
      return {
        id: v.id,
        kind: 'tool',
        name: p.name,
        // The provider's call id: names the tool call (one card per call,
        // upserted in place as it goes call -> result).
        call_id: p.call_id,
        // `args` is a JSON value (payload.rs `Value`): only an object has kv
        // lines to render.
        args: p.args && typeof p.args === 'object' ? (p.args as Record<string, unknown>) : undefined,
        // `output` is the model-facing channel: plain text, or an image block
        // (#34) the card shows as an attachment, never as a text line.
        output: typeof p.output === 'string' ? p.output : undefined,
        // GUI-only detail (#243): a wire-absent field (a tool that does not
        // split, or a pre-#243 session line) stays undefined.
        ui_detail: typeof p.ui_detail === 'string' ? p.ui_detail : undefined,
        status: decodeToolStatus(p),
        // The sidecar's bytes are gone (blob deleted from disk): the record is
        // what it is — say so, never fake a still-running call.
        unavailable: v.payload == null && v.blob != null ? true : undefined
      };
    }
    case 'om': {
      const o = freeform(v.payload);
      // The record's active_observations is the whole managed suffix; the
      // block shows what this entry added — the newest observation, past
      // its message boundary.
      const all = fieldOf(o, 'active_observations');
      const m = all.lastIndexOf('--- message boundary (');
      const nl = m >= 0 ? all.indexOf('\n\n', m) : -1;
      const raw = nl > 0 ? all.slice(nl + 2).trim() : all.trim();
      return {
        id: v.id,
        kind: 'om',
        text: stripObservationGroup(raw),
        thinking: optionalOf(o, 'om_thinking') || undefined,
        input: optionalOf(o, 'om_input') || undefined,
        suggestedResponse: optionalOf(o, 'om_suggested_response') || undefined,
        model: optionalOf(o, 'om_model') || undefined
      };
    }
    case 'system':
      return { id: v.id, kind: 'system', text: armOf(v.payload, 'system', readSystem).note };
    case 'spawn-snapshot':
      return {
        id: v.id,
        kind: 'spawn-snapshot',
        text: armOf(v.payload, 'spawn_snapshot', readSpawnSnapshot).log
      };
    case 'subagent': {
      const p = freeform(v.payload);
      return { id: v.id, kind: 'subagent', text: JSON.stringify(p), payload: p as SubagentPayload };
    }
    case 'diagnostic': {
      // ADR-0034: the payload is the tagged fact; the card composes the
      // sentence from the code. A code this build does not know keeps its
      // name and renders generically.
      const p = freeform(v.payload);
      return {
        id: v.id,
        kind: 'diagnostic',
        // The card composes the sentence from the fact; `text` is only the
        // code, so a list preview of an unknown code still says something.
        text: typeof p.code === 'string' ? p.code : 'unknown',
        diagnostic: typeof p.code === 'string' ? (p as unknown as Diagnostic) : { code: 'unknown' }
      };
    }
    default:
      return { id: v.id, kind: v.kind, text: JSON.stringify(freeform(v.payload)) };
  }
}

// #303: a stream delta carries only the new characters, so the view owns the
// accumulation. seq numbers the frames, and a delta applies only as the next
// one in a run: a duplicate, a gap, and a frame that opens a run already past
// its first are all refused, because appending them would repeat text or place
// it out of order. A refused frame freezes its run — the view never guesses a
// seq — and the entry's file line lands as an upsert, which is the recovery:
// a lost frame costs visible text, never the record. Returns whether the delta
// applied.
export function applyDelta(
  entries: Record<string, Entry>,
  seqs: Map<string, number>,
  ev: { id: string; seq: number; text: string; reasoning: string }
): boolean {
  const last = seqs.get(ev.id);
  if (last === undefined ? ev.seq !== 1 : ev.seq !== last + 1) return false;
  const card = entries[ev.id];
  const live = card && (card.kind === 'message' || card.kind === 'interrupted') ? card : undefined;
  const reasoning = (live?.reasoning ?? '') + ev.reasoning;
  entries[ev.id] = {
    id: ev.id,
    kind: 'message',
    text: (live?.text ?? '') + ev.text,
    reasoning: reasoning || undefined,
    calls: live?.calls,
    usage: live?.usage
  };
  seqs.set(ev.id, ev.seq);
  return true;
}

// Sidecar blob (ADR-0005): an oversized payload arrives null with a blob
// pointer; the payload is fetched on demand and substituted before decode.
// The blob file holds the stored map, not the wire's tagged shape, so the
// fetched bytes are tagged by the entry's kind on the way in.
// A fetch that fails keeps the pointer — one unreadable blob must not sink
// the window read.
export async function resolveBlobs(
  views: ViewEntry[],
  fetchBlob: (v: ViewEntry) => Promise<unknown>
): Promise<ViewEntry[]> {
  return Promise.all(
    views.map(async (v) => {
      if (v.payload !== null || !v.blob) return v;
      try {
        return { ...v, payload: payloadFor(v.kind, await fetchBlob(v)) };
      } catch {
        return v;
      }
    })
  );
}

// ADR-0008: the only live op on the transcript. The event's view is the
// session file's own line, so the decoded entry takes the map slot for its
// id — a first sight creates the card, a re-emission updates it in place;
// the slot's position is the id's (the creation order), so it never moves.
export function upsertEntry(entries: Record<string, Entry>, v: ViewEntry): void {
  entries[v.id] = decodeEntry(v);
}

// The persisted tool output records a failure: the bash executor prefixes
// "exit N" (non-zero N) or "bash: <error>" (spawn/timeout). Every other tool
// prefixes a failure with its own name ("read: cannot read ...") and the
// dispatcher prefixes routing failures; a success never does, so the prefix
// is a reliable failure discriminator.
export function toolStatus(name: string, output: unknown): 'running' | 'ok' | 'error' {
  if (output === undefined || output === '') return 'running';
  const s = typeof output === 'string' ? output : '';
  if (name === 'bash') {
    const m = /^exit (\d+)/.exec(s);
    if (m) return m[1] === '0' ? 'ok' : 'error';
    if (/^bash: /.test(s)) return 'error';
    // A completed bash result always leads with `exit N` (success or non-zero)
    // or `bash: ` (spawn/timeout/stop). A non-empty output with neither prefix
    // is in-flight streaming (#197): the command is still running, not done.
    return 'running';
  }
  if (s.startsWith(`${name}: `)) return 'error';
  if (/^(error:|unknown tool:)/.test(s)) return 'error';
  return 'ok';
}

// The core's explicit status (B5) wins when present; the prefix heuristic is
// the back-compat fallback for records written before it existed. A running
// call has neither an explicit status nor an output, so it falls through to
// toolStatus's 'running'.
function decodeToolStatus(p: ToolPayload): 'ok' | 'running' | 'error' | 'cancelled' {
  const s = p.status;
  if (s === 'ok' || s === 'error' || s === 'cancelled') return s;
  return toolStatus(p.name, p.output);
}

