# Technical debt — additive-fix audit (2026-11-03)

Scope: `crates/tau-core/src` (28,744 prod LOC), `crates/tau-acp/src` (2,606), `crates/tau-protocol/src`
(2,654), `app/svelte` (10,043, excluding generated `.svelte-check/`). Test files read as evidence only.

Method: six scoped read-only agents (om+om_integration; harness ×2; agent; subagent; frontend; tau-acp)
plus my own independent pass over `hashline/`, `session/`, `provider/`, `config/`, `plan/`, `tools/`,
`tau-protocol` and the frontend. **Every finding below was re-verified by me against the code** — file:line,
verbatim, with `git` provenance where a fix round is claimed. Nothing in this document was edited into the
codebase; the audit is read-only.

The question is one: **is logic present that was added to compensate for an invariant that is not
structurally enforced?** A finding is a compensation when deleting it is safe *only because a guard stands
where a type should*. The fix is always the same move: put the invariant upstream (type, state machine,
construction site, producer) so the bad state is unnameable, then **delete** the guard. Every proposal is
net-negative in production LOC.

> **Corrected by measurement, 2026-11-04.** The three implementation lanes that followed this audit shipped
> **net +308 production LOC**, not −923. Per-item line prices are estimates, and the pattern of their misses
> is the finding: items that deleted a *file* hit their price almost exactly (Add6-C1 −11/−11, Add6-C3
> −3/−3, FE10-ModelMenu −10/−12); items that touched a *type* grew the tree (4C +120, 4E +77, 1A +79, 1B +156).
> A guard deleted at its owner is a real deletion; expressing the invariant that deletes it costs lines. The
> governing bar became "is this genuinely an improvement to the codebase overall" — the tranches delivered
> **8 live defects fixed** and the line count is reported, not optimised. Ledger: **Measured Results**.

## Baseline (mechanical gates)

- `cargo clippy -p tau-core --all-targets` — **clean, exit 0**. CI denies warnings, so every finding here is
  a design issue, not a lint issue.
- **Test baselines are contention-sensitive.** A full `cargo test -p tau-core --lib harness::` run during this
  audit reported 79 failures, while the same tests pass in isolation (`tests_model_facts` 6/6, `tests_watcher`
  8/8). Six agents ran worktrees and test binaries concurrently; treat a large red sweep as suspect and
  re-run scoped before reading a regression into it. The per-finding red claims in this report are the
  agents' **measured scratch-tree** results (agent 43be2787 ran 179 agent tests to green as its baseline),
  and `harness::tests_pump` is **4/4 green at HEAD** as of this writing.
- 691 ticket-cited comments in tau-core production code. 376 guard-shaped; 222 swallow sites
  (`unwrap_or` / `.ok()` / `let _ =`): harness 76, subagent 39, hashline 31, tools 21, agent 20.
- fix:feat commit ratio crate-wide **68:20 = 3.4:1**; on `harness/state.rs` 7:3.
- 17 prod files over the 500-LOC soft limit: `hashline/mod.rs` 971, `hashline/registry.rs` 942,
  `harness/state.rs` 893, `tools/impls.rs` 868, `harness/launch.rs` 675, `agent/mod.rs` 658,
  `harness/sessions.rs` 651, `config/mod.rs` 636, `agent/turn/mod.rs` 618, `dispatch_sessions.rs` 562,
  `om_integration/mod.rs` 548, `harness/forwarding.rs` 534. The CI hard ceiling is 1000.
- The frontend store layer has **zero** `$derived`. That is the language, not laziness: verified with
  `compileModule('export const d = $derived(1)', {filename:'x.svelte.ts'})` → `derived_invalid_export`.
  Legal mechanism: a `$state` getter on the bag, or `$derived` in components (84 exist).

## The accumulation, quantified

`git log --numstat` on the config-rejection arc (#359 → #363 → #364 → #366 → #419), one file:

```
886762a  +28  -2     314eda7  +21  -7
bc89508  +22 -19     0e876d6  +37 -25
13c2b74  +12  -4     28a25b3  +68 -10
d271ff8  +15  -9     ────────────────────  state.rs TOTAL: +203 / -76  (net +127)
```

Seven commits on one proposition — *"this workspace's merged config, and the failure it is riding"* —
spread across two maps, a `""` sentinel key, an O(workspaces) fan-out, three self-healing clears, and a
40-line attribution function. `314eda7`'s own body states the rule the type does not hold: *"a latch and a
mark each describe a single meeting, and a superseded one is retired with what it described."*

Same shape, frontend: `Transcript.svelte` 63 commits, **+1488 / −1013** (net +475).
The counter-example, frontend: the virtua rewrite `4de8b19` **+108 / −638** (~6:1 deletion).

---

# Root cause 1 — state does not travel with the thing it describes

The deepest pattern in the repo, at **nine independent sites**. A fact about X is stored in a side table
keyed by X's id, so every operation on X owes it manual bookkeeping, and "keep them in step" becomes a rule
the code maintains by hand and the tests enforce.

## 1A · `Core` is 11 side tables, and `close_workspace` already missed one — **A**
`harness/state.rs:61,62,69,73,88,93,97,102,105,110,137` — 11 `Mutex<HashMap<String,_>>`; 52 lock sites;
lock ordering carried by a prose comment (`state.rs:775`: *"parking_lot::Mutex is not reentrant, and the
ordering invariant is cheaper to honor than to document"*).

`close_workspace` (`state.rs:596-612`) hand-sweeps **8** maps by id. **`skills` is keyed by `ws.id`**
(`sessions.rs:35/39/56`) and is **not swept** → closing a workspace leaks `skills[ws.id]` permanently. There
is no internal `Workspace` aggregate — `Workspace` is the protocol snapshot type only. `Drop for Core`
(`watchers.rs:418-428`) repeats the pattern as a 5-map `.clear()`. `tests_workspace_close.rs` has **zero**
watcher assertions.

**Shift:** one `Workspace` aggregate owning the 7 workspace-keyed maps, `Drop` stopping the 4 watchers.
`close_workspace` = one `remove`; `Drop` = one `clear`; the leak class is unconstructable. **Net −25.**
Red: a watcher-count assertion in `tests_workspace_close.rs` (red today — it is the leak).

## 1B · `configs` + `config_stale`: two maps for one proposition — **A** *(convergent: two agents, independently)*
`state.rs:62` + `state.rs:69`. **13 production touchpoints** (9 in `state.rs`, 4 in `watchers.rs`), two locks
per read, plus the `""` sentinel key for the system layer (`state.rs:789`). The mark-bookkeeping tax is paid
at 10 sites: `state.rs:308, 610, 694, 710, 756, 764`; `watchers.rs:158, 193, 199, 216`.

The tell that the invariant lives in the tests, not the type: **6 test sites poke `core.config_stale`
directly** (`tests_config_rejection/opening.rs:171,210,216`, `boundary.rs:223`).

**Shift** (the stronger of the two proposals): one map, one variant —
`enum ConfigState { Current(Config), Stale { running: Config, rejection: ConfigRejection } }`. A `Stale`
entry *carries* its rejection, so the mark cannot outlive the merge it describes; drop, `close_workspace`
and `put_merged` become single-map writes; the fan-out loop and the four `.remove(&String::new())` delete.
**Net −20.** Red: `tests_config_rejection/opening.rs:271`, `boundary.rs:223`, `tests_config_reload.rs:268`.

## 1C · `Registry` is 5 path-keyed maps that `evict_path` sweeps by hand — **B**
`hashline/registry.rs:44,46,49,51,60` (`owned`, `file_state`, `served`, `last_used`, `file_hint`), swept
one-by-one at `:420-424`. `owned` is a reverse index (anchor → path) over `file_state`'s keys — a second
representation of one relation. `refresh_owned` (`:242-264`) recomputes `checksums` the caller already has
(`tools/impls.rs:615` + `:623` pass the same `content` twice); `adopt` (`:209-226`) is a third partial
`touch`+`owned`+`served` write path. `file_hint`'s doc (`:55-60`) admits it *"lives within a single read
(the read's `evict_path` drops it afterwards)"* — intra-operation state in a session-lifetime map, with
`mint`'s `unwrap_or(self.probe)` ×2 (`:349,:354`) reaching into it. Severity **low**: `Registry` is
per-session (`agent/mod.rs:613`), freed on drop. **Net −33.** Red: `hashline/registry.rs` tests,
`hashline/properties.rs`.

## 1D · The OM buffer mirror: a vec *and* its own max, beside the record that owns them — **A** *(−60, largest item)*
`om_integration/mod.rs:37-47`. The doc self-indicts (`:110-112`): *"the record is the single source of truth
for the buffered chunks, **the in-memory vec its mirror**."* `buffer_cursor` *is* `max(range.1)` of those
chunks. **Every write to it in the codebase is a re-derivation** — verified: `from_config:119`,
`commit_chunk` (`background.rs:175`), `sync_buffer_mirror_from_durable` (`drain.rs:108`),
`merge_turn_end:210/214`. `drain_to` filters the same kept indices out of **two** vecs (`:153-167`) because
they are two vecs. `sync_buffer_mirror_from_durable` is a repair pass that exists only because the settle
runs on a clone.

The cost lands on the tests: `agent/tests_ladder.rs` carries **six** sites of
`om.buffered = vec![…]; om.record.buffered_chunks = om.buffered.clone();` — test code paying the invariant.

**Shift:** `OmState { config, record }`; the record is the one store. `buffer_tail()`/`buffered()` become
accessors; `drain_to` one `retain`; `merge_turn_end` a union into `record.buffered_chunks`;
`sync_buffer_mirror_from_durable` deleted. **Net −60.** Red: `om_integration/tests/record.rs:316`, `:355`.
No-op proof: `agent/tests_om_join.rs:336`.

## 1E · `child_handles` — a third copy of a relation the tree already owns — **B**
`state.rs:137` duplicates handle→parent, which also lives in `sup.children` (`subagent/mod.rs:302`) and on
disk (`subagent/restore.rs`). Two bridge callbacks exist **only** to keep the copies in step across #250:
`reclaimed` (`forwarding.rs:430`) removes, `re_adopted` (`:436`) re-mints. `live.agent.child_link()` already
yields parent **and** handle. **Shift:** derive by scanning sessions for
`child_link().handle() == handle`; closed children fall back to the disk record. Deletes the map,
`register_child_handle`, and both trait methods. **Net −20.** Red: `subagent/tests_reclaim.rs:276-299`,
`tests_dispatch.rs:33`.

## 1F · `Skeleton` validates its own freshness at the read site — **B**
`harness/snapshot.rs:113-118` — `count`, `load_seq`, `last_assistant`. The doc admits the borrowing
(`:109-110`): *"**`load_seq` is the store's load generation**"* — a cache in `LiveSession` comparing numbers
against a fact `SessionStore` owns (`session/mod.rs:280` bumps it). **Shift:** move the row cache into the
loaded log, cleared by `open()` in the same statement that bumps `load_seq`, appended in the append path
(`file.rs:181`). **Net −16.** Coverage gap: `tests_snapshot.rs` has **zero** `project()` calls — the
invalidation arm is guarded only by the e2e leg `app/tests/e2e/stress.spec.mjs`.
**Legitimate, keep:** the invalidation itself (an external writer grows the file). The defect is the
**location** — read site, not write site.

## 1G · `plans` is a Core-wide session-keyed map, so `refresh_plan` needs a `silent` flag — **B**
`state.rs:93-101`, `:621-658`. `changed` is **legitimate** (the OS watcher fires on an identical-bytes
save; #177 forbids UI churn). `silent` is debt: it exists only so the session-open path can warm a cache a
per-session fact should not need. Four teardown sites for one fact: `state.rs:637`, `launch.rs:301`,
`dispatch_sessions.rs:276`, `watchers.rs:427`. **Shift:** the observation lives in `AgentSession`
(`observe_plan()` emits on change); map, flag and teardown delete. **Net −15.**
Red: `tests_plan.rs:408-411` (the seed leg), with `:284`/`:389` pinning the pair.

## 1H · `Mapper` is 7 id-keyed collections hand-swept, one of them an index of another — **B**
`tau-acp/src/mapper.rs:21-38` — `text_sent`, `reasoning_sent`, `delta_seq`, `in_flight: VecDeque`,
`in_flight_set: HashSet`, `finalized`, `tool_created`. `in_flight_set` is a membership index over
`in_flight`, maintained at 4 sites (`:95-96`, `:126`). `on_stream_end` (`:123-131`) hand-sweeps **five**
collections per id. **Shift:** one `EntryProgress` struct per id in one map; `on_stream_end` = clear the map;
the set is a method on the deque. **Net −25.** Red: `tau-acp/tests/acp_protocol.rs`.
**Legitimate, keep:** the `delta_seq` gate (`:67-71`) — a lossy/batched transport is an external actor.

## 1I · Frontend: four mirrors of the same disease — **A**
Same rule, TypeScript side: key state by the identity it describes, let reads derive the rest.

- **FE2** 4 `activeFile*` fields (`state.svelte:94-97`), 11 write sites, **3** hand-written stale-fetch
  guards (`viewer.svelte.ts:59, 97, 108`) + a `resetActiveFile` ritual; `ChangesView.svelte:15-20` spends
  four `$derived` on one concept. **Shift:** `activeDoc: Record<key, Doc>` — a keyed write **is** the guard.
  **Net −17.** Red (new, and it is the test the guards bought): fetch A → switch to B → resolve A; today A
  clobbers B.
- **FE1** `entryOpen` is a `Map` (Svelte 5.57 deep-proxies objects, not Maps) → `EntryCard.svelte:42-66`
  keeps a local `$state` seed, a **dual write** (`:52-53`), a `thinkSynced` flag, and an `$effect`
  (`:60-66`) whose body is pure state synchronisation — **AGENTS.md, "Svelte", first rule.** **Shift:**
  `Record<string, boolean>`; the override-wins rule becomes the read. **Net −14.**
- **FE4** `pane()` returns null because creation lives in `ensurePane()`; 23 call sites, 16 needing
  null-handling, and **two verbatim rule-violating effects** — `LeftPane.svelte:20` and `RightPane.svelte:23`,
  both exactly `$effect(() => { if (ws) ensurePane(ws); })`. **Shift:** seed the pane at the two workspace
  producers. **Net −10**, and it deletes the codebase's only state-creation `$effect`s.
- **FE6** the windowing one-shot is keyed by session **id** while the state is keyed by object **identity**,
  repaired by a shadow `openedObj` Map + a 6-line unwind (`Transcript.svelte:182, 217-232`; #49/#42).
  **Shift:** key inside the policy. **Net −7.**

**Root cause 1 total ≈ −219 net, and it is one discipline.**

---

# Root cause 2 — the producer holds the fact and sends a notice without it

## 2A · `SpawnNotice` carries 6 strings; the bridge re-derives the other 6 facts and guards each — **A**
`spawn()` has `agent`, `provider`, `name`, `created`, `store`, `stop` **in hand** at the notice send
(`subagent/spawn.rs:143,168,184,207,210` — the map insert precedes the notice). `SpawnNotice`
(`subagent/bridge.rs:35`) carries `parent, handle, child, agent_type, context_mode, model` — none of them.

The bridge re-looks-up each fact and guards each lookup with a **silent** `else { return }`
(`harness/forwarding.rs:352-371`): `sup.child_agent`, `sessions.get(&parent)`, `SessionStore::open`. Every
guard can only fire if the producer is broken. Worse, the hand-off provider registry (`:273`, insert `:299`,
`take_provider` `:309-310`) exists solely to smuggle the provider across the notice, and its
`unwrap_or_else` fallback **constructs a second, non-agent provider** (`:391-401`) — manufacturing defect
#175, whose test `tests_stream.rs:552-556` asserts that a mismatched provider yields **no `StreamEnd` at
all**.

**Shift:** `SpawnNotice` carries `agent`, `provider`, `title`, `cwd`, `created`; the bridge registers what it
is handed; the factory is stateless. **Net −39.** Red: `tests_stream.rs:156`. Deleting the 3 guards is the
**no-op proof** — with the notice carrying the values, there is no lookup left to fail.

## 2B · The child's provider stop flag is **wired to nothing** in production — **A** (highest-severity concurrency)
My own find; 2A's defect expressed as a live bug.

`ForwardingProvider.stop` is load-bearing: `ForwardSink::event` cuts the stream on it (`forwarding.rs:147`
`let stopped = self.stop.load(…)`, `:153 if stopped { return false }`), it gates the prefill teardown
`stop_signal()` (`:158-160`, the 50 ms poll), and it gates the `completed` note (`:148-151`:
`completed[call_id] = true` is recorded **only when not stopped**).

- **Root path:** provider minted with `Arc::new(AtomicBool::new(false))` (`sessions.rs:297`) →
  `LiveSession.stop = provider.stop.clone()` (`:323`). **Shared**, so `live.stop.store(true)`
  (`dispatch_commands.rs:102`) reaches the provider.
- **Child path:** `ForwardingChildFactory::create` mints a **fresh** `Arc::new(AtomicBool::new(false))`
  (`forwarding.rs:296`) and stashes it (`:299`). The bridge sets `LiveSession.stop = agent.stop_flag()`
  (`:420`) and reuses the **stashed** provider (`:390-393`). `grep '.stop.store'` shows **no writer** for a
  child's provider flag. **It is an orphan.**
- **Consequence:** for a child, the provider-side stream cut never fires, the prefill teardown never fires,
  and `completed[call_id]` is recorded `true` on a stopped child — so 6B's reconciliation reads an
  interrupted entry-less child call as a **clean completion**. That is exactly the symptom
  `dispatch_commands.rs:103-107` warns about ("a user stop renders as a normal completion"), for children.
- **Why tests miss it:** the fallback (`:391-401`) wires the provider to `agent.stop_flag()` (`:400`) — the
  *correct* wiring — and the fallback runs when `factory: None` (`:346`). Suites that install a custom
  factory (`with_child_factory`: `tests_stream.rs:84/107/192/286`, `tests_model.rs:84`) do not stash
  (`take_provider` is inherent to `ForwardingChildFactory`, absent from the trait `bridge.rs:15-17`), so
  they exercise the **wired** path. **Production takes the orphan path; the child-stop tests take the wired
  path.** `tests_stream.rs:156`'s own docstring says "a non-forwarding test factory falls back to a fresh
  one" — the code knows the fallback is the test shape.

## 2C · `AgentSession::launch` and `ForwardingProvider::new` each mint their own stop flag — **A**

> **Partly fixed; the root half is a live defect, proven by execution (2026-11-05).** Lane A's `a719111`
> closed the **child** path: `Supervisor::spawn` mints the flag once, `ChildProviderFactory::create` takes
> it as a parameter, and `SessionParams.stop` carries it into the agent — the child's
> `provider.stop == agent.stop_flag()` now holds by construction, asserted in `subagent/tests_stop_flag.rs`.
> **The root path is untouched on main:** `harness/sessions.rs:263` mints the root provider's flag and
> `harness/launch.rs:470` mints the root agent's, so `dispatch_commands.rs:102`+`:107` still write **two**
> flags under the prose comment. Proven by adding `a_root_sessions_forwarding_seam_stops_on_its_agents_flag`
> (an `Arc::ptr_eq` of the root's `live.provider.stop` against `live.agent.stop_flag()`) to a tree at main
> `240deec`: **FAILED**, while the child's identical assertion passes. `SessionRole::Root` gaining a minted
> `stop` field is the fix.
>
> **The "9 paired write sites" count is wrong** (lane A's measurement): `queue.rs:26/86/129` write the
> queue's own flag with no partner, and `dispatch_commands.rs:22`, `dispatch_sessions.rs:286/319`,
> `forwarding.rs:512`, `launch.rs:639` write a single alias alone. **The only true pair is
> `dispatch_commands.rs:102`+`:107`.**
`agent/mod.rs:554` mints `stop: Arc::new(AtomicBool::new(false))`; `forwarding.rs:48` takes a *separate*
one. Nothing mints one flag and hands it to both, so each construction site stitches them.
`SessionRole::Root` carries no stop field, which is *why* `dispatch_commands.rs:102` + `:106` must write
**both**, under a 4-line comment naming the failure. That is the 9 paired write sites
(`dispatch_commands.rs:22,102,106`; `dispatch_sessions.rs:291,324`; `launch.rs:630`; `forwarding.rs:513`;
`queue.rs:26,86,129`).

The child path is the codebase's own proof that the pair is not "two mechanisms": `forwarding.rs:420`
writes `stop: agent.stop_flag()` — one flag, shared.

**Shift — 2A + 2B + 2C are ONE ticket:** mint the flag **once** at the site that creates the session and
pass it into both `AgentSession::launch` (a role field) and `ForwardingProvider::new`, so
`provider.stop == agent.stop_flag() == live.stop` by construction; `SpawnNotice` carries the provider.
Deletes: the 9 paired writes + their comment; `LiveSession.stop`; the stash map + `take_provider` + the
14-line fallback provider; and the orphan flag. **Net −53 combined** (−39 / −14). This is also the fix that
makes 6B's ledger trustworthy for children.

## 2D · `skills_of` discards the value its producer returned, then re-reads the key it just wrote — **B**
`harness/sessions.rs:34-40`. `refresh_skills` (`:49-64`) builds `reg`, inserts it, emits it, **and returns**
the skills. `skills_of` discards the return, re-locks, re-reads the key it just wrote, and
`unwrap_or_default()`s. Verified unreachable-by-construction: exactly **3** `skills.lock()` sites (2 get, 1
insert), nothing removes. A miss turns a successful discovery into a refused `/skill:` send —
`unwrap_or_default()` converts success into "no skills" with no trace. **Shift:** return what
`refresh_skills` built. **Net −3** — small, and a clean Shape 1+4. **The no-op proof is the point:**
deleting the re-read and the default leaves the suite green.

## 2E · `self_weak`: 7 sites silently drop work on a state the constructor makes impossible — **A**
`state.rs:138`. `build()` is the sole `Core` constructor and sets the weak **in the same function**
(`state.rs:253` init, `:285` `replace(Arc::downgrade(&core))`). The codebase admits `None` is unreachable —
`sessions.rs:283` writes `.expect("session_new on a built core")` — yet **7 sites** degrade silently:
`watchers.rs:58, 125, 269, 310`, `forwarding.rs:518`, `launch.rs:635`, `sessions.rs:283`, each
`let Some(core) = … else { return Ok(()) }` — **reporting success while dropping a watcher registration**.
Mirror in `subagent/mod.rs` (`self_weak` + `reclaim.rs`'s direct read). **Shift:** `build()` returns
`Arc<Self>` and hands it to registrations. **Net −18.**

---

# Root cause 3 — a repair pass on the wrong side of the seam

## 3A · `repair_dangling_tool_calls`: 67 production lines, **zero test coverage** — **A**

> **Refuted as stated (2026-11-05, wave 2).** "Zero test coverage" is false: `harness/tests_turn_guards.rs:334
> a_reopened_session_repairs_dangling_tool_calls` drives the exact defect — writes an assistant leaf whose call
> has no tool result, closes and re-opens, asserts the repair mints the missing tool entry with
> `status: "error"`, then asserts the session accepts a new message and settles. That is a positive and a
> recovery assertion on the production path. The one literal claim that holds is narrower: the minted *string*
> (`sessions.rs:468`) is not asserted by any test, so the message text is free to drift. The shift's real
> argument is the surviving one — a second implementation of settle logic on the consumer's side, with silent
> `Ok(())` arms — not the coverage claim, so severity drops from A to B and the −55 price is unverified.
`harness/sessions.rs:455-521`, called from `:250` at session reopen (#267). Crash recovery is legitimate —
the crash is an external event. The debt is that it is a **second implementation of settle logic** on the
consumer's side of the seam, with silent arms: `let Ok(payload) = … else { return Ok(()); }` (`:470`), a
non-assistant leaf → `Ok(())`, an empty `calls` → `Ok(())`. The repair quietly does nothing and the next
turn re-sees an open call.

Verified coverage: the minted string `"turn was interrupted before tool execution completed"` appears at
`sessions.rs:500` and **nowhere in any test**; the function has one caller. A 67-line production repair pass
with zero tests is the strongest available evidence that it is compensation, not a state model.

**Shift:** mint the tool-result slots **before** awaiting the tools — `append_line` is synchronous
(`session/file.rs:164`), so the loop records the slots as it records the assistant entry and extends them as
tools complete. `TurnSettleGuard` is the model. A crash then leaves a closed record by construction.
**Net −55.** Red: hand-write a file whose leaf is an assistant with 2 `calls` and no tool entries; assert
reopen mints 2 `status:"error"` entries today, and that the same file needs no minting after the shift. The
negative (a healthy file stays byte-identical, `:243-244`) is preserved.

## 3B · `mint_id` swallows the open the invariant depends on — the 80 GB regression is patched, not fixed — **A**
`session/mod.rs:221-230`:
```rust
pub fn mint_id(&mut self) -> String {
    // Mint only after the file is known: an unloaded store starts at
    // 0 and would re-issue ids an existing session already holds
    // (duplicate id -> cyclic parent chain; see the live-om 80GB
    // regression). A failed open surfaces on the following append.
    let _ = self.ensure_open();
```
The comment names a catastrophic failure and the guard is `let _ =`. An unloaded store has `next: 1`
(`:154-160`), so `mint_id()` returns `"00000001"` — the first entry id of **every** existing file. The
collision is **exact, not probabilistic**. Compensation downstream: `unwrap_or(0)` ×2 (`mod.rs:323`,
`file.rs:185`) and the O(n) cycle guard `session/branch.rs:18-23`.

Mostly harmless on the append path, because `append_line` runs `ensure_open()?` first (`file.rs:135`) and
the line is never written. **The escape is `agent/queue.rs:97-98`:** `mint_id()` → `upsert_event(&id, …)`,
which is `&self`, fires the wire entry event with **no** open and **no** validation — so a duplicate id
reaches the GUI (ADR-0008: wire id == file id), and `queue.rs` keeps that id to re-write at dequeue (#148).

Tests cover the **symptom**, never the cause: `session/tests.rs:442`
`a_fresh_handle_mints_past_the_files_ids` and `tests_branch.rs` (the "80GB runaway" amplifier). **No test
asserts `mint_id` cannot return a duplicate — because the type permits it.**

**Shift:** `mint_id(&mut self) -> Result<String, Error>`. The open-before-mint invariant moves into the
signature. All 8 callers verified to be in `Result` context (`agent/queue.rs:97`;
`agent/turn/tools.rs:47,78,113` — in `cancelled_tool_entry`/`run_tools`, both `Result<(), AgentError>`;
`agent/turn/mod.rs` in `run_turn`/`append`; `harness/sessions.rs:490`; `session/file.rs:256`). Deletes the
`let _ =`, its 4-line rationale, and both `unwrap_or(0)`. **Net −12.**
**Keep:** `branch.rs`'s cycle guard — external CRC-valid corrupt files are real, and `entry.rs:112-114`
exists precisely to build them in tests. That one is legitimate.

## 3C · `chunk_covered` cleans up a state its own producer is permitted to build — **B**
`om_integration/turn_end/mod.rs:9-25` (#395) re-checks at promote time that a chunk's end is above the
observation cursor, with a `_ => false` parse fallback. Producers: `commit_chunk` cuts its range from
`unobserved_slice(…, buffer_cursor)` (`background.rs:190`), already past the cursor; **`fork_record`
inherits parent chunks whose ends are ≤ the parent cursor** — the exact case `fork_record` creates and #395
names. Its tail: the O(n²) `spent` set (`:84-86`) and `drain_to`'s count-0 branch.
**Shift:** `fork_record` drops chunks behind `parent.cursor` (it already re-bases `last_buffered_at_tokens`,
same decision). Guard, walk, spent-set delete. **Net −9.** Red: `tests/promote.rs:132`, `:255`.

## 3D · The archive's child set is built three ways and reconciled with a prune — **B, corrected**
`dispatch_sessions.rs:361-404`: the supervisor arm, the live-map arm, then the disk scan whose `archived`
branch **`children.remove`s** what the first two added; `sessions_archive.rs:89` filters `path().exists()`
**again**. ADR-0005 says the file is the record, so the disk scan is the one truth.
**Net −7 — not −21.** I checked the agent's claim of a "verbatim duplicated child loop":
`sessions_archive.rs:72-86` runs **after** the up-to-5 s quiesce window and re-checks `cl.turn`; the
dispatcher's loop (`:398-404`) is the pre-check. They are a legitimate pre/post pair, not duplicates. The
supervisor arm's `ChildState::Running` refusal (`:377-379`) is likewise **not removable** — the live `turn`
flag only tracks GUI sends, per the code's own comment.
Red: `tests_archive.rs:249` (the disk arm is load-bearing). No-op proof: `tests_archive_edges.rs:283`.

## 3E · Frontend: three repair transitions for a state no producer creates, and a GUI bug — **A/B**
- **FE7** `turn: 'starting'` is a GUI-invented **third arm** of a two-arm wire enum (`protocol.ts:342`
  `'idle' | 'running'`, mirroring Rust `enum TurnState { Idle, Running }`; grep 'starting' in
  `tau-core`/`tau-protocol` → zero occurrences). Minted client-side (`composer.svelte.ts:28`); three sites
  exist to heal it (`events.svelte.ts:110`, `:175`), and consumers widen for it (`StatusBar.svelte:9`,
  `rules/windowing.ts:18`, `Transcript.svelte:36`). `:175` is pure Shape 3: a failure before any
  `stream_start` strands a state nothing produced. **Shift:** "awaiting first output" is the *absence of
  output in a running turn* — data the store has. `turn` returns to the wire's arms; `awaiting` is a getter.
  **Net −4.** Red: `live-stream.test.ts:63` goes red the moment `:175` deletes, proving the repair load-bearing.
- **The compaction event is delivered to the GUI and silently discarded** — **A (GUI bug)**.
  `SessionEventKind::Compaction { entry, first_kept }` (`events.rs:199`) is emitted by the core
  (`harness/turn.rs`, `sessions.rs`, `snapshot.rs`), declared in `protocol.ts`, and spec-mandated (§191 the
  session event group is "branch move, compaction, archive"). `events.svelte.ts:138-140` handles
  `session_event` with **one arm**: `if (ev.kind.kind === 'branch_move')`. So the GUI receives a compaction
  event and drops it. **The variant is NOT deletable (spec §191); `first_kept` is** — 0 prod readers on
  either side of the wire. Fix = handle the event (the record's divider renders via `presentation.ts:278`
  once the entry lands; the event's job is the re-fetch/tail nudge).

---

# Root cause 4 — typed data round-tripped through strings

## 4A · The ACP mapper throws away the typed payload it is handed — **A** (AGENTS.md rule violation)
`ViewEntry.payload: Option<Box<EntryPayload>>` is **typed** (`tau-protocol/src/snapshot.rs:110`);
`EntryPayload` is a tagged enum (`payload.rs:227-229`, `#[serde(tag="type", content="data")]`) with arms
User/Assistant/Tool/System/SpawnSnapshot/Other. `tau-acp/src/mapper.rs` **never matches it.** All three
handlers call `entry.payload_value()` (`snapshot.rs:122-126`: typed → `serde_json::to_value` → `Value`) and
string-parse the fields back out with silent defaults:
- `:112` `payload.get("text").and_then(Value::as_str).unwrap_or("")` — `UserPayload.text` is `String`, not `Option`.
- `:142` the same for assistant `text`/`reasoning` (both `String`), driven by a table of **string field
  names** (`:138-141`).
- `:162` `name` (`String`), `:163` `args`, `:187` `rawOutput`.
- The real defect: `ToolOutput` is `#[serde(untagged)] enum { Text(String), Image(ImageBlock) }`
  (`payload.rs:101-103`). The mapper recovers that **discriminant by shape-sniffing** — `as_str()` is `Some`
  for `Text`, `None` for `Image` (`:164-166`).

The helper's own doc self-admits it: *"the consumers that read fields by name — the ACP mapper … does not
model the entry taxonomy"* (`snapshot.rs:117-119`). #346 typed the payload; this consumer kept the pre-#346
free-form view.

**Shift:** `match entry.payload.as_deref() { Some(EntryPayload::User(p)) => …p.text…, … }`. The
`unwrap_or("")` ×3, the string-name table and the shape-sniff delete. Prod consumers of `payload_value()`:
mapper ×3 + `pump.rs:1` (tau-core harness *tests* index it heavily and stay). **Net −10** (roughly neutral —
a match adds arms). Taken for the same reason as 4B: it deletes a bug class, and it is AGENTS.md's
*"everything across the IPC boundary is a typed protocol value; no raw `serde_json::Value`."*

## 4B · `om_status` round-trips a typed enum through a bare `&str` with a catch-all — **A**
`agent/mod.rs:322` `pub(crate) type OmStatusHook = Arc<dyn Fn(&str) + Send + Sync>;`. The producer holds a
typed `TurnEndAction` and maps it to strings (`turn_end/mod.rs:278-283`); the consumer maps strings back to
the typed protocol enum with `_ => OmStatusKind::Idle` (`launch.rs:519-525`). The `&str` in between buys
nothing; **any new or misspelled activity kind reports "idle"** — a GUI that silently stops showing the
memory pass. Same violation at `launch.rs:614-617` (`Some(n) => n.to_string(), None => "killed"`).
**Shift:** `OmStatusHook = Arc<dyn Fn(OmStatusKind) + Send + Sync>`; a new kind becomes a compile error at
the producer. **Net −4, and it deletes a class of bug.** Red: `tests_om_async.rs:242`; the negative is a
one-character producer typo with **zero red** today — the proof the guard is not load-bearing.

## 4C · `subagent/restore.rs` hand-parses an enum that already has serde — **B**
`restore.rs:179-186`: `match s { "compacted" => Some(ContextMode::Compacted), "fork" => …, _ => None }` then
`.unwrap_or(ContextMode::Fresh)`; `:177` `.unwrap_or("general")`; `:144,148`
`title.unwrap_or_else(|| m.id.clone())` and `handle.unwrap_or_else(|| m.id.clone())`. `ContextMode` **already
derives `Deserialize` with `#[serde(rename_all = "lowercase")]`** (`states.rs:148-150`), and the producer
writes it from the typed value (`spawn.rs:87-94`). `EntryKind::Subagent` has no typed payload —
`payload.rs:276` lumps it into the untyped arm. **Shift:** a typed `SubagentPayload` beside the five that
exist; the hand parser, the table and the defaults delete. **Net −12.** Red: `subagent/tests_*.rs` restore legs.

## 4D · Frontend `decodeEntry` re-verifies types the wire fixes twice — **B**
`lib/wire/entries.ts:62-63,94,124` — 15 `typeof` re-checks (`:85-215`). `dataOf` folds the *typed* arm and
the `other` escape into one untyped shape, so every typed arm must re-verify. But `protocol.ts:34-40,53-61`
type these **non-optional** (`UserPayload.text: string`), mirroring Rust's non-`Option` fields
(`payload.rs:16,17,34,35,45,53,54,55,86,87,178,185,195` — `pub text: String`, `pub lane: String`, no
`#[serde(default)]`). **Shift:** `dataOf` returns `PayloadOf<T>` by construction for the typed arm (one cast
at the fold); the defensive reads stay **only** in `freeform()`/the `default:` case, where the core genuinely
cannot type the payload. **Net −11.** Red: extend `entries.test.ts` (11 tests today).
**Keep:** every `??` read of a real `Option` (`usage`, `stop_reason`, `ui_detail`, `source`, `skill`) and
every `freeform()` guard.

---

# Root cause 5 — a domain wider than its type, so it is guarded three times

## 5A · `buffer_activation: f64` — a ratio that is the inverted copy of the knob it came from — **A**
`om/mod.rs:28`. Verified independently by me and by the om agent: **it is not a config key** (grep empty in
`config/` and `tau-protocol/`); the only production construction is `om_config_of`
(`om_integration/mod.rs:95-103`), which derives the ratio **from** `buffer_increment`. Every consumer then
inverts the ratio **back** into `buffer_increment` (`background.rs:25,39,202`). A lossy round-trip
(absolute → ratio → absolute), invented and un-invented.

Consequences, all verified verbatim:
- `retention_floor`'s `>= 1000.0` branch (`om/mod.rs:102`) is **dead in production** — reachable only from
  `om/tests_core.rs:96-102`, testing a config the app cannot construct.
- `expect("guarded >= 1000.0, far below u32::MAX")` (`:106`) asserts a bound the branch does not establish:
  `1e18` passes `>= 1000.0` and fails `u32::try_from`.
- Three `#[allow(clippy::cast_*)]` blocks (`:73-79`, `:108-114`) justify themselves with *"stays within
  [0, observe_threshold]"* — **provably false** for the absolute branch.
- `clamp(0.0, 1.0)` at two sites; `observe_threshold.max(1)` where the loader already refuses zero.

**Shift:** `NonZeroU32` fields. `retention_floor() = observe_threshold − buffer_increment`;
`trickle_floor() = buffer_increment.get()/2`. Delete the f64 field, the ratio math, the dead branch, the two
clamps, the `.max(1)` and all three `#[allow]`s. **Net −20.** Red: `tests/record.rs:4`; no-op proof:
`om/tests_core.rs:96`'s absolute half.

## 5B · The u32 token domain: stated in a validator, asserted in an `expect`, saturated in a third place — **B**
`om_integration/mod.rs:48-52` `threshold_u32(v: u64) → .expect("om thresholds are validated to fit a u32 at
config load")` — an `expect` on a `u64` parsed from TOML, whose own doc admits the arrangement. Plus
`drain.rs:33` `.unwrap_or(u32::MAX)` and `config/sections.rs:316-336`'s validation loop with
`OmThresholdZero` (`:323-324`, added by `e015fe5` precisely because *"a zero observe_threshold drops the
settle's plan gate"* — a production guard pinned because the type permits a zero).
**Shift:** `NonZeroU32` in `config::Om`; the overflow fails `Deserialize`. `threshold_u32`, `.max(1)`, the
validation loop and the saturation delete. **Net −8.** Red: `config/tests/layers.rs:21`.

## 5C · The entry id is a `u64` wearing a string, so **seven** production sites re-parse it — **A**
`session/entry.rs:31` `pub id: String`, minted `format!("{:08}", self.next)` from `next: u64`
(`session/mod.rs:65,227`). Seven production sites re-parse it to compare order, each with a fallback:
`session/mod.rs:323` and `session/file.rs:185` (`next.max(id.parse::<u64>().unwrap_or(0) + 1)`),
`om_integration/mod.rs` (`max_by_key(…parse().ok())`, `ranges_overlap`'s `span`), `turn_end/window.rs` ×2,
`turn_end/mod.rs` (`chunk_covered`). **3C, 3D and 5D are downstream of this root.**

Measurement that scopes the fix honestly: **181 `.id` touchpoints across 37 prod files**; 155 prod
signatures carry `id: &str`/`id: String`/`-> String`. A crate-wide newtype is **not** a net deletion. Two
facts narrow it:
- Ids are `{:08}` zero-padded, so **lexicographic order == numeric order** while all ids are 8 wide. The
  codebase knows: `session/tests.rs:457` asserts `minted > last.id` — a **string** compare, green. So the
  five `om_integration` parses buy **nothing** over a plain `str` compare.
- **A bare string compare is not safe in principle:** lex order breaks at 100,000,000 entries
  (`'99999999' > '100000000'` is `True`) and `u32::MAX` is 10 digits. So `EntryId(u32)` with numeric `Ord` is
  strictly *more* correct than the string compares **and** strictly safer than the parses, which drop the
  `Ok` on overflow and fall back.

**Shift (scoped):** `EntryId(u32)` + `IdRange { first, last }::contains` **at the om_integration seam only**
(`BufferedChunk.range`, `Cursor.entry_id`) — 5 parse sites + `ranges_overlap` + `last_chunk_end` +
`chunk_covered`. **Not** across the 37 files.
**Hard disk-compat constraint:** `BufferedChunk.range: (String, String)` is `Serialize`/`Deserialize` and
**durable** inside `OmRecord.buffered_chunks` (`#[serde(default)]`, #86 P2). `IdRange` must serialize as the
same 2-tuple of `{:08}` strings, custom serde at that one boundary. `OmRecord` has **no version field** —
only `#[serde(default)]` tolerance — so a format change looks survivable while silently not. **Net −10.**

## 5D · `ranges_overlap`'s equality fallback answers "disjoint" for the pair it cannot order — **A**
`om_integration/mod.rs:230-241`, widened from `c.range == range` by `f124475` (#402):
```rust
let span = |r| (r.0.parse::<u64>().ok(), r.1.parse::<u64>().ok());
match (span(a), span(b)) {
    ((Some(a0), Some(a1)), (Some(b0), Some(b1))) => a0.max(b0) <= a1.min(b1),
    _ => a == b,
}
```
Both sides of a **production** range are always `(span.first_id, span.last_id)` (`background.rs:211`,
`commit.rs:141`) — 8-digit ids, always parseable. So `_ => a == b` is reachable only from fixture-shaped ids:
a test accommodation living in production, and for a mixed pair it answers **"disjoint"** — re-enabling the
double-append the function exists to prevent. Its own pin records this: `tests/record.rs:418` asserts
`!ranges_overlap(&r("e1","e1"), &r("00000001","00000002"))`.
**Shift:** 5C's `IdRange::contains`; the fallback and all four parses delete. **Net −10.** Red:
`tests/record.rs:406`, which is simultaneously the no-op proof — its last three assertions pin *the fallback*.

## 5E · `plan`'s guard justifies itself with a case the loader rejected one day earlier — **B**
`turn_end/mod.rs:395-401` returns `Done` citing *"a legal `observe_threshold = 0`"*. The zero-refusal
(`sections.rs:323-324`) landed **2026-11-03** (`1bc3b03`); the comment landed **2026-11-02** (`b3d5980`). The
premise was dead inside 24 hours. Same mechanism as `sessions_archive.rs:119-120`, whose hazard `4e470fd`
closed on 2026-09-25, **eleven days after** `5cf1270` wrote the comment (2026-09-22).
**Guard-comment rot is systematic: cite code, never prose.**
**Shift:** `observe_head` returns `Option<(ObservedSpan, String, u32)>`; `plan` maps `None → Done`;
`ObservedSpan::of` takes a non-empty input so both `expect`s (`commit.rs:29,32`) delete. **Net −6.**
Red: `tests/plan.rs:323`, `:355` (the second hand-builds the config the loader now refuses).

## 5F · Three `AtomicBool`s whose predicate is re-typed at four sites — **B, corrected**
`agent/mod.rs:497-505` — `kill`, `stop`, `closed`, 11 hand-written `Ordering::SeqCst` sites.
**A lifecycle enum is the wrong fix and I am rejecting the proposal that suggested it.** Verified: the three
have three *different lifetimes*, documented in place — `kill` is **per-call** (reset at each call start,
`mod.rs:140` & `:265`), `stop` is **persistent until the next send** (`queue.rs` sets true on stop, false on
send), `closed` is **one-way** (`SessionClose`). `kill` and `stop` are on different clocks: no
Running/Stopped/Killed/Closed enum expresses "killed for this call, stopped for the session".

The real debt is narrower: the predicate `kill.load() || stop.load()` is re-typed at **4 sites**
(`sink.rs` ×2, `turn/mod.rs:53`, `:177-180`), and the post-call `cut_by_user` (`:177-180`) re-reads all
three flags that `turn_killed` (`:53`) read before the call, with a `kill.store(false)` (`:140`) in between —
so the two reads are deliberately *different facts* that work only because of site order. **Shift:** one
`CutFlags` value type with `fn cut()` + `fn cut_by_user()`, minted by the session and handed to the sink (it
already takes `kill.clone(), stop.clone()`, `mod.rs:148-150`). Deletes 4 duplicated disjunctions + the
3-line prose that explains them. **Net −10.**
Red: the cut/stop matrix in the agent tests, plus a new assertion that `cut_by_user` and `cut` disagree
exactly across a `kill.store(false)`.

---

# Root cause 6 — re-checks, mirrors and defensiveness that manufacture the case they handle

## 6A · `run_turn`'s delta read turns a storage failure into an **empty delta** — **A** (the only finding that destroys data)
`harness/turn.rs:95` `let start = store.leaf().ok().flatten().map(|e| e.id);` and `:115-121`:
```rust
Some(cursor) => store.entries_since(&cursor).unwrap_or_default(),
None => store.entries_range(0, usize::MAX).unwrap_or_default(),
```
`store.open()` **succeeded at `:51` with a full error path**, so `leaf()` cannot fail here: `.ok()` converts
a can't-happen into "the session's first turn", and the `None` arm exists to handle that manufactured case —
with a comment claiming *"the store has no read wider than a cursor and narrower than a position range
(#302)"* while the very next line calls `entries_range`, a read of **exactly that width**. Both
`unwrap_or_default()` calls make a storage failure an **empty delta**: the turn's `StreamEnd`, tool
`EntryUpsert`s and `Compaction` events silently vanish — the #261/#82 "session showed running" symptom,
arriving as data loss on the record-derived wire (ADR-0008). `entries_since` (`session/mod.rs:396-414`)
re-reads the whole file, so it **can** fail after `open()` succeeded.
**Shift:** `leaf_cached()` + `entries_since_cached()`, propagated through the existing `:98-112` error-emit
path. **Net −4; severity is the highest in this report.** Red: seed a session, open it, `chmod 000` the
file, send, settle — today `StreamStart` and **no** `StreamEnd`; `tests_turns.rs:143-151` already asserts
against that shape, and the fault path has no coverage.

## 6B · The k-th entry ↔ k-th call join: a shadow ledger that knows less than the record — **A**
`turn.rs:96` `calls_before`, `:125` `assistant_index`, `:137-143` the positional join, `:204-226` the
entry-less-call pass; `forwarding.rs:30-36` `calls: Vec<String>` + `completed: HashMap<String,bool>`. The
fact `StreamEnd { call_id, interrupted, usage, duration_secs }` exists **twice**: on the record
(`AssistantPayload`, `payload.rs:52-72`) and in the provider's ledgers, joined by position under an
invariant the code states but does not own (*"the kth entry in a turn is the kth call"*). Verified tell at
`turn.rs:217-224`: `usage: None, duration_secs: None` — **the ledger knows less than the record**, and
`completed.get(&call_id).copied().unwrap_or(false)` silently reads an unknown call as cut. `completed`
exists only because content-free assistants are suppressed (`agent/turn/mod.rs:519-523`, *"no content is no
entry"*), making "cut" vs "completed-empty" unrecoverable from the log.
**Shift:** one `CallEnd { id, interrupted, usage, duration_secs }` ledger, appended at call start and
finalised at the `Completed` frame or at kill; exposed as `take_call_ends()`. Position matching,
`calls_before`, `assistant_index`, `completed` and the second block delete. **Net −28.**
Red: `tests_turns.rs:100-151` (a call aborted during the 60 s prefill → **interrupted** `StreamEnd`) and
`:466-481` (a completed entry-less call must **not** read interrupted). `tests_stream.rs:472-566` (#175)
pins the fragility: a child whose `LiveSession` provider differs from its agent's yields an empty `calls`
and a lost `StreamEnd` — a second source that **can desync** (see 2B: in production it does).

## 6C · `LiveSession` carries four copies of facts the session owns — **A**
`state.rs:39-54`, 7 fields, 4 redundant: `stop` (9 paired writes, see 2C); `provider` (3 construction sites
must pass the identical `Arc`; `tests_stream.rs:552-566` exists because they diverged); `cwd` (4 copies);
`skeleton` (1F). **Net −14:** `LiveSession { meta, agent, turn }` + `stop()`/`cwd()`/`provider()` accessors.
**Spec correction (mine):** `leaf` is spec-mandated as a *snapshot* field (§186, "leaf pointer"), so the
**protocol** field is not deletable — it is simply **dead weight as a `LiveSession` field** (2 Rust writes,
1 frontend write, 0 reads in Rust, 0 reads in the frontend). `snapshot()` already derives the true leaf from
the store (`snapshot.rs:27 store.leaf_cached()?`) and ships it as `Snapshot.cursor` (`:72`), while
`SessionMeta{..meta}` (`:41`) ships the **hand-synced cache**. Populate the wire field from the derived leaf
at the snapshot site; delete the `LiveSession` field and its 2 sync writes. Spec satisfied, the 3-site sync
problem gone, and the field can never be stale because it is computed.

## 6D · `pending_tokens`: six writers, three different quantities, one value that is not what its name says — **B**
`om/mod.rs:222`, written at `turn_end/mod.rs:137` (chunk tokens), `:214`, `:244` (raw), `commit.rs:154`,
`drain.rs:81`, `om_pass.rs:426,451`. #392's fix moved every **gate** to `unobserved_tokens(store)`
(`turn_end/mod.rs:53`, comment at `:87-89`: *"The removal target is computed against the raw window, not the
stored field"*) — the field survived as a projection each producer fills differently. Read by two consumers
(`drain.rs:65,82` and `harness/snapshot.rs:56`). `mod.rs:137` is **dead in production**: verified that the
settle overwrites it at `:244` whenever `promoted`, and the snapshot read is the only other consumer of the
stale value. **Shift:** delete the field; `pending_tokens(&self, store) -> Result<u32, OmError>` at the read
sites; `promote` takes `raw` as a parameter (both callers already computed it). **Net −12.**
Red: `tests/promote.rs:39` (documents the decoy outright: *"`pending_tokens` is the caller's decoy for the
stored field"*), `tests/settle.rs:288`, `:615`.

## 6E · Two more hand-synced mirrors in OM — **B**
- `prefix_demoted: bool` (`mod.rs:44`) is written true at exactly one site, under `if raw_tokens <
  config.observe_threshold` (`turn_end/mod.rs:179-190`) — it duplicates its own setting condition. Read once
  at the *other* site (`:237`), where the producer has fallen through to the promote branch; the branch
  already carries the fact. `plan` is the sole producer (all writers verified), so a `plan` return value
  carries it. **Net −13.** Red: `tests/settle.rs:288`.
- `observation_tokens` (`mod.rs:221`) is a hand-synced copy of `token_count(record.active_observations)`:
  written in lockstep at both mutation sites (`om_pass.rs:426,451`) and **read from the durable record at
  the drain site** (`drain.rs:62,75`) — the two representations are already read from each other's
  location. `refresh_gate` is pure over the record and `OmState` is session-local. **Net −6.** Red:
  `drain.rs:171`, `:185`.

## 6F · `workspace_config_checked` re-reads a map 24 lines after reading it, in a race with a watcher thread — **B**
`state.rs:756-768`. The loop body reads `config_stale` (the `?` arm), then reads **the same map** at `:766`
and calls `remove` on the entry it just read. A watcher fires on the interval between (`watchers.rs:208`,
`state.rs:694`), so the removal can target an entry a live watcher inserted after the check — dropping its
mark. Benign (the next boundary re-reads), and the doc states the design. The `None` arm is
unreachable-by-construction: every `workspace_config_checked` read site guarantees the key exists
(`state.rs:704-710`, `watchers.rs:193-199`). **Shift:** one read, act on the binding (a `Checked` enum).
**Net −4.** And `workspace_config_silent` (`:682-689`): `.0` discards a config-load failure, making
"this config is broken" indistinguishable from "there is no config" — it exists only because `ConfigState`
is absent (1B); it deletes with it. **Combined −16.**
Red (new): a staleness test where the watcher marks the config *between* check and clear; today the clear
can drop a live mark.

## 6G · Frontend defensiveness — **B**
- **FE3** see 4D. **FE5** the new-activity pill keeps three hand-synced scalars (`newCount`, `lastSignal`,
  `signalSession`) plus a magic-base change signature `$derived(all.length * 100000 + lastLen)`
  (`Transcript.svelte:116-135`), all mutated **inside an `$effect`** — a relation between two snapshots,
  i.e. a derivation, not a mutation; the `* 100000` encoding silently caps at 100 000 tail characters.
  **Shift:** one effect records the *outside-world* snapshot (scroll position); the count derives. **Net −9.**
- **FE8** `subagent_event` detaches the row it just mutated and re-reads it with `!`
  (`events.svelte.ts:148-152`): two mutation styles (pure-return in `rules/`, in-place in the fold) meet in
  one `switch`, 7 of 8 cases use the in-place `s` and one must discard it; `applySubagentEvent` **already
  receives `now`** and already writes `last_modified` for the child (`rules/sessions.ts:394`). **Shift:** the
  parent touch belongs in the function that owns the fold. **Net −3.** Red: `events.test.ts:163`.

---

# Not findings — legitimate bounded-cost and external-world handling

Adjudicated as debt-shaped but **not** debt. Listed so the next reader does not re-litigate them; several
were proposed by agents and rejected on evidence.

**Rust.** `PipeCounters` (`state.rs:432`) — counted drops + a summary on recovery, a bounded-channel design.
`MAX_DRAIN_SLICES` / `MAX_REFLECT_SLICES` / `MAX_OM_TICK_SLICES` — the model is external and can return a
degenerate answer; a cost bound is the only defence, and the cursor snapshot is what makes the cut
deterministic. `idle_gap_secs`'s kind-skipping walk — the real rule, documented against the file layout.
`transcript`'s `unwrap_or(0i64)` / `unwrap_or(UNIX_EPOCH)` — an untrusted `u64` from a CRC-verified line,
with no upstream constructor that bounds it. `entry_text`'s `unwrap_or_default()` — payload-shape absence.
`provider/overflow.rs` (`is_overflow_body`, `OVERFLOW_SIGNATURES`, the proximity parse) — the upstream
protocol **is** unstructured text; no type can absorb an HTTP error body, and deleting the heuristic crashes
(its comment records a near-miss: *"the gateway's prose sits two characters from its window number"*).
`hashline`: pure functions, a bounded pool, counted reclaim; `reclaim`/`drain_reclaimed_warning` with
`MAX_RECLAIMED_REPORTED` + `+N more` is the `PipeCounters` pattern; `persist_pins`' `let _ =` and
`pin_store::load`'s None-on-anything are documented best-effort caches with the consequence stated ("the
caller re-mints"); `expect("all anchored")` (`mod.rs:665`) is locally provable by the loop at `:645-661`, and
AGENTS.md permits `expect` on a proven invariant. `TurnSettleGuard`'s Drop and the archive's
flag-before-move are the repo's **model of doing this right**. `refresh_if_grown`, the watcher
rescan-on-empty-batch, the temp-rename index write, plan content-dedup, `unique_name`'s bounded search,
`canned.rs` (debug-only, 9 swallows, every caller a test). `branch.rs`'s cycle guard (3B) and
`viewer.svelte.ts`'s `isBinaryError` (the core's UTF-8 rejection *is* the signal).
`session/file.rs`'s exists-check (`:164-168`) stays closed by `4e470fd` — the **comment** above it is dead.

**Frontend.** The three `JSON.stringify` change-guards (`rules/files.ts:35`, `events.svelte.ts:52,164`) —
verified against the producers: full-state re-emit from the core watcher (#31) and the OS watcher (#177).
**Do not delete.** The seven `now > last_modified` guards — protecting age labels from an NTP back-step, with
the single-clock discipline documented at `:73-76` and `events.test.ts:187` *asserting* which events do not
touch it (a hoist would need a parallel "counts as activity" set: a Shape-2 net **increase**).
`streamClock`'s seq refusal — a real lossy transport, with the file line as recovery. `resolveBlobs`' catch.
Fullscreen tracking (`state.svelte:128-152`) — Tauri has no fullscreen-changed event
(`tauri-apps/tauri#7162`). Request coalescing (`windowsInFlight`, `blobsInFlight`, `inFlight`).
`noUncheckedIndexedAccess` `?? []` reads.

---

# Scope covered, and what is not

Production LOC only (test files, `tests/` dirs and `tests_*.rs` excluded — they are not part of the
net-deletion claim). Workspace production total: **36,021**.

| Swept by the eight passes | prod LOC |
|---|---|
| `harness/` | 7,038 |
| `agent/` | 3,559 |
| `subagent/` | 3,428 |
| `hashline/` + `session/` (Addendum 4) | 5,107 |
| `tau-protocol/` | 2,654 |
| `tau-acp/` | 2,606 |
| `om_integration/` | 2,241 |
| `om/` | 1,443 |
| frontend store layer (`app/src/lib/*.svelte.ts`, wire types) | ~4,000 of 17,749 |
| **Swept** | **~28,076 — 78%** |

**Not swept, in descending likelihood of findings:** `provider/` 2,315 (`overflow.rs` adjudicated
legitimate; the rest untouched), `tools/` 1,970 (includes `impls.rs` at 868, a soft-limit file — and
Addendum 4's H1/H3 land in it from the hashline side, so it is implicated but not swept),
`config/` 1,145, tau-core top-level 1,212 (`diagnostics.rs` 369, `context.rs` 247, `agent_type.rs` 226,
`gate.rs` 211), `skills/` 323, `plan/` 300, `tau-mock-llm/` 680, and the frontend **component tree**
(~7,700 of 17,749; FE1–FE8 came from the store layer and a component subset).

The findings are therefore a **floor, not a ceiling**: exhaustive for the 78% the passes read line by
line, silent for the rest. Wave 4 passes 2 and 3 (`provider/`+`config/`+top-level; `tools/`+`tau-mock-llm`+
frontend components) are in flight; their findings append as addenda, not merged into the
totals in the Verdict.

Two honesty notes on the finding count. Several of the 51 are sub-findings of one root (1A–1G), and
several of my own claims were refuted and stay in the report **as** corrections (my 4A had the right site
with the wrong mechanism; 3B dropped A→B; 5F's enum proposal is refuted; `in_flight_set` is worth −3, not
−12). Only a handful of sites got genuine independent rediscovery — `config_stale` (two agents, same
site), the side-table sweep (three scales), `self_weak` (three sites), the subagent record (two agents,
same root), the entry id (two agents, both declining the same scope). Single-sourced findings — all of
tier B — are the ones that moved under verification.

---

# Addendum 1 — the `agent/` audit (agent 43be2787), folded in 2026-11-03
This pass is the strongest methodologically: it ran the suite in a scratch copy and **measured** each red by
deleting the guard, so its no-op proofs are results, not arguments. Baseline `cargo test -p tau-core --lib
agent::` = 179 passed. Findings below are numbered into the root causes above.

## 1J · Four hand-latched "record once" ledgers, three rollback implementations, **one of them missing** — **A**
`agent/mod.rs:371,411,414,418` — `om_stall_recorded: bool`, `gate_gap_recorded: Option<String>`,
`om_failed_recorded: Vec<String>`, `config_rejection: Option<(String,String)>`. Each mirrors a durable
diagnostic record and must be rolled back by hand when the append fails — the code says so at
`turn/om_pass.rs:327-331` (*"`om_stall_recorded` mirrors the record once the append lands"*):
- `om_pass.rs:338-365` — set latch (`:353`) → append → on `Err`, clear it by hand (`:365`).
- `om_pass.rs:299-315` — push latch → append → on `Err`, `retain(|s| s != stage)`.
- `gate.rs:236-257` — `fresh` computed under lock, latch set at `:245` → `self.diagnostic(..)?` at `:254`
  propagates **with no rollback** (the one latch whose rollback is missing).
- `config.rs` (`config_rejection_changed`, `config_accepted`) — a fourth copy with a clear-on-success rule.

**The bug:** a `record_gate_gap` whose append fails leaves the latch **set forever** — the diagnostic is
permanently suppressed for that model, with nothing recorded and nothing re-tryable. Four keys (unit /
model / stage / (file,reason)), four latch policies, three rollback implementations, and the fourth is
absent. This is the compensation signature exactly: "record this once, and undo it if the write fails" is
one operation that four sites each re-implement.

**Shift:** one `DiagnosticLatch` in `Inner` with
`fn once(&mut self, key: impl Into<String>, fact: &Diagnostic, store) -> Result<(), AgentError>` — set,
append, **clear on `Err`**, return `Ok`. The rollback-on-failure rule exists once, so the missing arm is
unconstructable. Keys `"om_stall"`, `"gate_gap:{model}"`, `"om_failed:{stage}"`, `"config:{path}:{reason}"`.
**Net −38** (≈55 deleted, ≈17 added). Red: `tests_ladder.rs:658`, `tests_om_stall.rs:75`,
`tests_config_boundary.rs:219`; positives at `tests_ladder.rs:698`, `tests_om_stall.rs:139`. **Not a no-op** —
all four latches are tested, so this is coordination debt, not dead weight.

## 6H · `AgentError::Om` — an error arm no producer can reach — **A** (dead code on a typed surface)
`agent/mod.rs:58,85`. `grep -rn "AgentError::Om" crates/ app/ acp/ eval/ fuzz/` → **zero construction
sites**, only the `Display` arm. `om_turn_end` (`turn/om_pass.rs:137-237`) declares
`Result<(), AgentError>` but absorbs every OM failure into a record + `hook("idle")` + continue (`:206-220`,
the #86 contract), and `write_back_om` swallows save failures into `tracing::warn!`. There is no
`From<OmError> for AgentError`, so the variant is **unconstructible on the loop's error path**.
**Shift:** `om_turn_end(&self) -> OmPassOutcome` — infallible, because #86 already says a failed pass never
kills the turn; the type should say what the code does. The `?` at `turn/mod.rs:232` and `gate.rs:121`
disappear with it. **Net −7.** **Measured no-op proof:** deleting the variant and both arms leaves all 179
agent tests green — the arm never fires.

## 3F · The #148 pre-mint is a second id lifecycle, and it is the escape hatch in 3B — **B**
`queue.rs:84-122` + `mod.rs:304-308` + `turn/mod.rs:505-510`. `queue_notified` mints the entry id at queue
time, fires the wire upsert, stores it in `Queued::entry_id: Option<String>`, and `append_user` branches on
it at dequeue. **Measured:** forcing `entry_id: None` (no queue-time mint; always mint at dequeue) keeps
**all 406** agent+harness+subagent tests green — including
`tests_stream::a_notified_steer_lands_in_id_order_while_the_parent_call_is_in_flight`, whose id-order
assertion (`tests_stream.rs:441-447`) compares file order to *sorted file order*.
The mechanism buys a GUI property (visible before the file line) that no agent-module test asserts.
**This is the same `queue.rs` mint that escapes `mint_id`'s swallowed `ensure_open` in 3B** — the Option
`entry_id` and the branch are the cost of a lifecycle the type does not express.
**Shift (honest):** a #148 decision, not an accident — do not ship unilaterally. Type
`Queued::Notified { entry_id, .. }` so "a notified steering has a minted id" is representable-only, deleting
the `Option` and the `match msg.entry_id` branch. **Net −6.** The deletion variant is −38 with **zero** reds
in the module: reopen on #148.

## 6I · `PromptAnchor` stores the whole content of the thing it prices — **C**
`turn/estimate.rs:14-24` — `model`, `leaf`, `instructions: String`, `tools: String`, `om_items: String`,
`input_tokens`: the anchor stores **serialized content** of the prompt parts and compares them at `:56-58`,
serializing the OM log **per call** via `fingerprint` (`:44`, `:79`). It works and `turn/tests_anchor.rs`
covers it fully. Its doc gives the reason content is the guard: *"a reflector can rewrite it without adding
or removing an item, so the content is the guard"* (`:21-23`) — a legitimate rule (an external model rewrites
the log), so the fix must preserve it, not delete it. **Shift:** keep the content-derived comparison, drop
the string clones — `AssembleStamp { model, leaf, plan_present, tools: u64, om_digest: u64 }` (xxh3 is
already a workspace dep, `tau-core/Cargo.toml:21`): a digest **of the same content** preserves the
reflector rule and compares in O(1), with the same fail-closed `price() → None`.
**Net −8**, and it deletes a prompt-serialization per call. Red: `tests_anchor.rs:363,378,392,414`.
Batch with the next touch.

## Refinements the measurements force on my text

- **2C, sharpened.** Deleting the *second* store at `dispatch_commands.rs:107` reddens exactly **one** test
  (`tests_queue::a_stop_with_queued_messages_leaves_them_and_starts_no_new_turn`, the #223 drain guard). The
  stream-cut half of stop rides `LiveSession::stop` alone (`forwarding.rs:147`; `tests_turns.rs:143` stays
  green). So the comment's claim — *"Both must be set, or a user stop renders as a normal completion"* — is
  true of the **entry-marking half at one site**, not of the four-line warning's general case. The single-flag
  shift stands and is stronger than I wrote: most of the paired writes are decorative.
- **5F, better mechanism.** The pre-turn snapshot `turn_killed` (`turn/mod.rs:50-56,177`) exists *because the
  same function clears `kill` at `:140`* — a second representation of "this turn was cut", bought back after
  the loop destroys it. `self.kill.swap(false, SeqCst)` at `:140` **returns the consumed kill for free**:
  hold it in a loop-local `cut_consumed` and `cut_by_user = (cut_consumed && first_call) || live flags`. Same
  two facts, no pre-turn snapshot. **Measured:** naive reduction reddens exactly
  `tests_turns::kill_policy_completes_the_inflight_tool_batch` (`tests_turns.rs:408`); the swap keeps it green.
  **Measured no-op:** the tail clear at `turn/mod.rs:263-265` — deleting it passes all 179 tests.
- **5F, the model is confirmed by a second independent pass.** The agent reached the same conclusion I did —
  **the 4-state lifecycle enum is wrong** — by enumerating all 21 `Ordering::SeqCst` sites: `kill` is per-call,
  `stop` is per-send, and a `ToolBatchPolicy::Complete` force produces *kill=true, stop=false, turn continues*,
  which `Running/Stopped/Killed/Closed` cannot name. The correct model is **two axes**:
  `AtomicU8 Interrupt { None, Stop, Closed }` (one-way; `closed` subsumes `stop`) + the existing per-call
  `kill`, with one `fn interrupted()` replacing the duplicated disjunctions at `sink.rs:68,141`,
  `turn/mod.rs:53,177-180`, `tools.rs:43`. **Net −14** (three flags → two, 21 `Ordering` sites → ~14), and
  "closed cleared by a send" (`queue.rs:26`) becomes unrepresentable. My `CutFlags` consolidation and this
  axis model are the same ticket; the axis model is the better root, mine the cheaper first step.
- **`closed` disjunct, measured.** Deleting `turn/mod.rs:177-180`'s `closed` disjunct reddens exactly
  `tests_turns::closing_a_session_stops_its_in_flight_turn` (`tests_turns.rs:71`) — which asserts the **event**
  `StreamEnd{interrupted}` from `ForwardSink::stop`, **not** the entry payload. No test in the workspace
  asserts the disjunct's own product. It carries the #229 claim and fires only in the ordering window before
  `stop` lands.
- **The gate ladder is not debt** (I had it on the list as adjacent): `turn/gate.rs:99-144`'s fingerprint
  comparison is the provider's *charged* fact once a call ran — re-estimating over a non-shrinking branch
  replaces a measurement with a guess and sends the doomed request (`ddd7acc`; `tests_ladder.rs:442,497`).
  Same for the #256 400-recovery (`turn/mod.rs:340-390`, `budget().unwrap_or(0)` at `:376`, pinned by
  `tests_overflow.rs:512`), the `om_pass.rs` join/re-clone dance (the honest cost of a guard that cannot
  cross an `await` plus a concurrent background cycle), `reported_cutoff` (`turn/mod.rs:578-592`, two facts
  from two actors, #229 at `tests_turns.rs:227`), and `same_file_edit_conflicts` (`tools.rs:9-36`; its
  duplicated parse walk is a B-level −12 simplification, not an invariant compensation).
- **Agent/ verdict, matching mine:** the compaction/ladder machinery absorbed six fix rounds (#392 → #394 →
  #399 → #402 → #408 → #409) **structurally** — fingerprints, bounds, durable facts — and reads clean. The
  debt is in the **seams**: the stop signal split across two owners, error/latch surfaces no producer
  reaches, and id/flag double-representations. Lowest-risk opening pass: A1 + A2 + B1, every step pinned by
  an existing test, no state machine required.

---

# Addendum 2 — the `subagent/` audit (agent e77a60e4), folded in 2026-11-03

Baseline: 2,530 prod LOC, 45 swallow sites, 68 ticket citations. The ledger is the accumulation made
visible — `restore.rs` + `snapshot.rs`, prod-only, per commit:

```
7590fbd #221  +319/−0   restore.rs born (disk fleet read, settled(), own_state)
544bf2e #250  +145/−2   DiskChild, handle_of map, mint_counter, self_weak, re-adopt
da823e3       +367/−349 split into restore.rs + snapshot.rs (net +18)
9e9505d #346  +116/−64  ChildStateRef mirror + to_protocol
90f6f30 #313  +56/−19   SPAWN_HEAD/STATE_TAIL windows, head-then-tail spawn search
                    ─────────────────────────────────────────────────────────
                    10 stacked rounds: +1,116 / −488 → the 377 lines those files hold today
```

## 4C is the same site, and it is bigger and buggier than I wrote — **A, convergent**
Two passes independently landed on `restore.rs`'s hand-decoded subagent record. My −12 was the local
symptom; this is the root. `EntryKind::Subagent` is **the only kind with no typed payload**
(`payload.rs:276` lumps it into the untyped arm) while the TS mirror declares it in full
(`protocol.ts:184-191`). So the shape is hand-decoded **three times** — `restore.rs:165-188`,
`snapshot.rs:50-73` (**verbatim duplicate**), `route.rs:22-30` — plus two more copies of the core→protocol
mapping (`restore.rs:55-59`, `snapshot.rs:80-84`) beside the owner `mode_to_protocol`
(`forwarding.rs:190`). `ContextMode` already derives `Deserialize` with `rename_all = "lowercase"`
(`states.rs:148-150`) and the producer writes it from the typed value (`spawn.rs:91`).

**The live defect:** `restore.rs:168-174` searches the head window **first**. A fork child's log is the
parent's copied tree (`spawn.rs:64-70`), and the parent's spawn records are written by
`spawn.rs:84-97` — so **a fork child of any parent that has ever spawned a sub-agent reads a sibling's
provenance.** `tests_reclaim.rs:128-177` passes only because its fixture parent has no spawn records; its
own comment concedes *"a head-only lookup would have read the general/fresh defaults."*

**Shift:** `SubagentPayload::{Spawn(SpawnRecord), State(ChildState), Notify{…}}` in `tau_protocol::payload`
— **probe-verified** in a scratch crate to round-trip every payload `child.rs:67-86` and `notify.rs:105-111`
write today, byte-compatible with existing files, and a truncated record is a hard `Err` (which is already
`ChildStateRef`'s rule, `snapshot.rs:148-152`). Deletes the three decoders, both mapping copies,
`set_state`'s 20-line payload `match`, and the defaults. **Net −52** (replaces my −12).
Red: `tests_reclaim.rs:128-177`. **No-op proof:** `snapshot.rs:338-381` + `:383-407` pass with **zero** lines
of `unwrap_or("general")` / `unwrap_or(ContextMode::Fresh)` — no test in the crate asserts those defaults.

## 1K · `handle_of` reads keys **no writer in the repository writes** — **A**
`restore.rs:91,108-118,145-148` builds a `child → handle` map from spawn records with `handle`/`child` keys,
then `unwrap_or_else(|| m.id.clone())`. **The only writer of `event:"spawn"` is `spawn.rs:87-94`, which
writes `type, brief, context_mode, parent, call` — no `handle`, no `child`.** Verified by grep across
`crates/` and `fixtures/`: `"handle":` appears in the module only as the *reader* at `restore.rs:113`.
So the map is **always empty in production**, the handle is always the session id, and the module has grown
rules to accommodate its own fallback: `mint_counter`'s `unwrap_or(0)` with *"an unparseable handle (a
re-adopted session-id handle) is 0 — the oldest, which is correct"* (`reclaim.rs:205-212`); a registry
inserted under a session-id key while the live registry keys on `parent-N` (`reclaim.rs:189`); and the
frontend deduping by `child` **as well as** `handle` (`rules/sessions.ts:336`).
**Shift:** `SpawnRecord.handle`, minted at `spawn.rs:119-123` — the mint is a pure `fetch_add` 5 lines above
the append, so no new state. The map, its scan, both fallbacks and the special-case rule delete.
**Net −10.** **The no-op proof is the test that names it:** `tests_reclaim.rs:94-124` asserts
`handle == "child-session"` and its own comment concedes *"absent, so the session id stands in"* — it passes
with all 11 lines of `handle_of` deleted.

## 3G · `set_state` commits memory unconditionally and the file fallibly, and the callers disagree — **A**
`child.rs:63-89` writes `*self.state.lock() = state.clone()` **first**, then appends fallibly. The module's
own contract (`mod.rs:13-14`): *"the child's file is the record; the in-memory state is its live view."*
An append failure leaves the two **permanently split**, and every reader (`child.state()`, `snapshot.rs`,
`restore.rs`, `fleet_view`) then disagrees. The compensation is three divergent Err policies at five call
sites: propagate (`lifecycle.rs:50,107`), log-and-continue (`lifecycle.rs:176`, `notify.rs:55`),
`mark_failed` + break (`spawn.rs:264-267`).
**Coverage: zero.** Grep for `"state entry failed"` in tests → no hits; each of the three arms is a
**measured no-op** (the module suite passes with any one deleted).
**Shift:** the append is the commit point — publish to memory **only on `Ok`**; all callers propagate.
**Net −11**, and the implementation must add the positive/negative pair the invariant needs
(append-failure → state stays prior; success → state advances). Same family as 6A and 1J: a swallowed
storage failure that outlives the call.

## 3H · `minted_count` seeds a mint cursor by counting a *tool* record's name string — **B**
`mod.rs:218-226` counts `EntryKind::Tool && payload["name"] == "subagent_spawn"` to seed the handle counter,
while the module already parses the real cursor in the inverse direction (`reclaim.rs:205-212`). Two
representations of one cursor, joined by a whole-log scan and a frontend safety net
(`rules/sessions.ts:355-365`). **Shift:** seed from `max(mint_counter(handle))` over typed
`SpawnRecord`s. **Net −6.** Red: `tests_spawn_handles.rs:26-98`; no-op proof: `:101-131`.

## 6J · Two more copies of facts their owners already state — **B**
- `SubagentInfo::waiting_on` (`mod.rs:379`, produced by `inspect.rs:28-33`) is a **derived copy** of
  `ChildState::Idle { waiting_on }`, carries an invented `Awaiting → User` rule restated at
  `lifecycle.rs:196-204` (#368), and **#346 erased it from the wire** (`events.rs:172-189` has no
  `waiting_on`; `forwarding.rs:201-221` drops it). One formatter reads it (`route.rs:86`).
  **Net −6.** No-op proof: `tests_notify.rs:276-391` passes with the whole derivation deleted — **no test
  anywhere asserts the `" (waiting on "` string.**
- `is_terminal()` (`states.rs:97-104`) is **the model of doing it right** — one owner, trusted at
  `reclaim.rs:72` and `restore.rs:140` with no surrounding guard. Its only debt is two re-spellings of the
  same triple: `mod.rs:306-313` (`!matches!(…, Done|Stopped|Failed)`, literally `!is_terminal()`) and
  `lifecycle.rs:36-39`. **Net −6.**
- `Child`'s 15 fields are hand-initialized at two sites (`spawn.rs:196-218`, `reclaim.rs:165-186`) — 11
  identical initializers plus **two deliberate differences** (`origin`, `state`) that exist only as literals
  in two files, with the invariant in prose at one of them (*"a re-adopt must not write a duplicate state
  entry"*, `reclaim.rs:173-177`). **Shift:** one constructor owning the field set. **Net −29**; the suite is
  the equivalence proof.
- `self_weak` confirmed at a **second** site (my 2E): `mod.rs:78-82,200,202` + the sole reader
  `reclaim.rs:137-140`. `Supervisor::new` is the only constructor and returns `Arc<Self>`, so post-build
  `None` is unreachable; the `?` silently drops a re-adopt of an on-disk child with no diagnostic.
  **Shift:** `new() -> (Arc<Self>, Weak<Self>)`, field becomes `Weak<Self>` — and `Weak::upgrade` takes
  `&self`, so the **mutex disappears** from the resolve→re-adopt path. **Net −5.**
  The right fix, stated against my 2E: **delete the `Option`, do not copy the `.expect`** — that trades a
  silent drop for a panic.
- `soft_bound`/`hard_cap` (`reclaim.rs:22-34`) both encode "an optional positive bound" with
  `map(try_from).filter(|b| b > 0)`; a `NonZeroUsize` config field makes zero unrepresentable and collapses
  both to 2 lines. **Net −6.** The `0` case is untested — the whole `tests_caps.rs` suite is the no-op proof.
- `spawn.rs:128-139`'s 8-draw name retry duplicates the owner `unique_name` (`harness/state.rs:354`, used
  by the top-level path at `sessions.rs:130-136`) — a **−11** dedup that also removes a real divergence
  ("gives up after 8 draws"). Legitimate bounded cost today; the duplication is the finding.

**Rejected by evidence:** `settled()` (`snapshot.rs:115-122`) is **not** a "is it finished" re-ask — it
rewrites a `running` record as `Idle{Parent}` because **the process died**. The actor is the OS, the rule is
stated once, and it has a positive and negative test (`:262-276`, `:313-328`). Not a finding. The
`inspect.rs` display swallows (9), the `SPAWN_HEAD`/`STATE_TAIL` caps, `child_plan`'s
`unwrap_or_default` (*"absence of observation is not proof of completion"*), the `#347` fleet lock, and the
drive-generation guard are all legitimate.

**Verdict for this module: patchwork on the disk-identity axis, structurally shifted on the state machine.**
The 6-state machine with one terminal predicate, one writer per transition, serde types with `From` impls to
the wire, and the drive-gen permit are the wins. Everything that reads a child's identity **from disk** is
hand-rolled, duplicated, and self-compensating. **Deepest change: one typed subagent record + the
identity/cursor sites that read it — −120 of 2,530 prod LOC (≈4.7%).**

## Provenance note: `96d0d5c fix(pump): the transport coalesces a frame's events into one emit (#425)`
Mid-audit, `harness/pump.rs` gained a 16 ms coalescing window (`FRAME` 16 ms, `MAX_BATCH` 64) with a new
`tests_pump.rs`, and ADR-0008 was amended by one line. This is **not** part of this audit and I did not
touch it; it is recorded because it interacts with a finding. ADR-0008:24,38 still read *"the delta
coalescer … deleted"*; the pump doc now says the pump *"is the transport's coalescer"*, scoped to
*N concurrent streams cost one IPC per frame* (the per-stream 16 ms cadence stays the turn loop's). The
scoping is coherent — the deleted thing was the per-stream delta coalescer, this is a transport-level batch
across streams — but **the ADR's prose is now stale in exactly the way §5E documents as systematic**:
`b251453` deleted the coalescer, `96d0d5c` added one, and §24/§38 assert the deletion.
Verified at HEAD: `cargo test -p tau-core --lib harness::tests_pump` → **4 passed**. (An intermediate
uncommitted state had 2 of 5 failing; the committed version is green.) **Action: amend ADR-0008 §24/§38 to
name the transport-level window, so the ADR and the code agree.** A spec that says "deleted" beside code
that adds one is the seed of the next compensating guard.

---

# Addendum 3 — `tau-acp` + `tau-protocol` (agent 406a434a), folded in 2026-11-03

Method note: two findings are proven by **running the shipped binary** — `target/debug/tau-acp` driven over
stdio against `tau-mock-llm`. Prod LOC measured (test modules excluded): tau-acp 1,946, tau-protocol 2,116.

## 4E · **PROVEN LIVE DEFECT** — the ACP tool-result latch freezes a streaming tool at its first chunk — **A**
`tau-acp/src/mapper.rs:170-182`, fields `:28,:31`. Verified verbatim:
```rust
} else if has_result && !self.tool_result_sent.contains(&entry.id) {
    self.tool_result_sent.insert(entry.id.clone());
    ...  "status": "completed",
```
fed by `:168` `let has_result = output.is_some_and(|o| !o.is_null());`

**The client transcript, reproduced:** a `bash` call streaming `chunk-1/2/3` at 400 ms yields
`tool_call(in_progress)` → **one** `tool_call_update(completed, "chunk-1\n")` → prompt response. The client
is told the tool **completed** carrying 1 of 3 chunks; the final result never arrives. Second scenario: a
**failing** `bash` (`exit 7`) is reported `"status": "completed"` — verified: `grep -n status mapper.rs`
returns 4 hits, all ACP's own string literals; **the mapper never reads `ToolPayload.status` at all.**

**Why it is compensation, not a bug in one line.** `tool_result_sent` asserts *"one result per tool id,
forever."* The core deliberately does not maintain that: `tools/progress.rs:76-113` (verified) re-upserts
**the same id** with `output: Text(growing)`, `status: None`, one frame per 16 ms, documented *"the final
result wins … the coalesced in-flight upserts are wire-only."* The invariant is true **in the file**, not
**on the wire** — the mapper's lane is full-state-per-frame, the rule `events.rs` states for `Queue`,
`PlanChanged`, `SubagentEvent`. The latch converts a legitimate stream into a one-shot, and the `has_result`
sniff makes the *call-phase* entry (`Text("")` → `""`, non-null) satisfy it, so the **first progress chunk
wins the latch.** Streaming is the `bash`/`read_stream` path (`tools/impls.rs:730-752`, verified). The
trigger is exact: the call-phase entry carries `output: ToolOutput::Text(String::new())`
(`agent/turn/tools.rs:55`, verified) → serialized `""` → non-null → `has_result` is true on the frame
*after* `tool_created`, so the update fires at the first frame carrying any output at all. The same site
writes `status: Some("cancelled")` for a cancelled call (`:13`) and `tools/mod.rs:91-99` derives `ok`/`error`
from the output text — **three encodings of one phase**, the exact set `ToolStatus` collapses.

**The tell:** `c841a51` — *"acp: simplify on_tool's has_result — drop the subsumed text disjunct (#118 A1)"*
— a 2-line commit **proving the semantics of a shape-sniff** across absent/null/string/object-or-array. A
proof of a sniff on a concept the type already names.

**The pin that hides it:** `crates/tau-acp/tests/acp_protocol.rs:700-703` asserts
`updates_for_call.len() == 1, "one tool_call_update for the call"` — **the suite asserts the defect as
intended behavior.** Any fix deletes that assertion in the same change.

**Shift — put the phase in the type, at the crate that owns it.** `ToolPayload.status: Option<String>`
(`payload.rs:96`) → `enum ToolStatus { InFlight, Ok, Error, Cancelled }` (snake_case, `InFlight` serializes
as today's absent status, so `protocol.ts:157` and the goldens hold). Construction sites are few and
single-owner: `tools/mod.rs:91-99` (the existing `status()` 3-branch `&'static str` chain),
`agent/turn/tools.rs:59,91,131,201`, `tools/progress.rs:106,150`, `harness/sessions.rs:503`. Then
`on_tool` reads the phase instead of guessing from the shape; `tool_created`, `tool_result_sent`,
`has_result`, `output_text` and the `rawOutput` default all delete — nothing is left to hand-sync, and the
client streams. **Net −44** (mapper −36, pump −3, frontend −24 = `entries.ts:279-304`'s prefix-heuristic
status classifier, protocol +19).
Red: `mapper.rs:398 tool_call_is_two_phase` goes red the moment `tool_result_sent` deletes. Add the missing
positive — *a streaming tool's final result reaches the client* — which is **red today**.
No-op proof: `mapper.rs:466` (the #265 blob test) passes with zero lines of `tool_result_sent`: it is a
single-upsert sequence, so it never touches the latch. It looks like coverage of the two-phase rule and is not.

## 4A is confirmed and sharpened — with one correction to my text
All 4 prod consumers of `payload_value()` (mapper ×3 + `pump.rs:284`) string-parse the payload #346 typed.
Verified: `grep -rn "EntryPayload::" crates` → **no prod consumer matches an arm**; the 8 hits are
`from_entry` constructors and the goldens. My `unwrap_or("")`-on-a-non-`Option`-field claim is exact
(`UserPayload.text: String`, `payload.rs:34`) — so for a typed arm the default is **unreachable by
construction**, and `mapper.rs:455` passes with all 7 deleted.

**Correction to my 4A:** I wrote that the mapper recovers the `ToolOutput` discriminant *for rendering*. It
does not — the create path never touches `output` and `rawOutput` rides the whole `Value` through. The
shape-sniff exists to decide the **phase**, which *is* 4E. My finding was the right site with the wrong
mechanism; 4E is the defect it was hiding.

**Second correction (mine, material):** the typed fix is not purely additive. A `match` on
`entry.payload.as_deref()` must handle `None` (the sidecar case today absorbed by `payload_value()` →
`Value::Null`), and #265's inlining (`launch.rs:664-671`) means `payload` is `Some` on the paths that
matter. A 1-line `let-else` returns `Vec::new()`. Priced with 4E (they share the three handlers): −44.

## 6K · `Usage` stores a derived sum that its **sibling type in the same crate** refuses to store — **A**
`tau-protocol/src/lib.rs:471-481` — `Usage { input_tokens, output_tokens, total_tokens, cached_prompt_tokens }`
under `#[serde(default)]`, beside:
```rust
// payload.rs:321-324
/// Derived, never stored: recording the sum beside the parts is the two
/// writers ADR-0034 refuses for a prose note next to typed fields.
pub fn total_tokens(&self) -> u64 { self.input_tokens.saturating_add(self.output_tokens) }
```
`payload::TurnUsage` = 3 fields + a method; `lib::Usage` = 4 fields, same concept, **same crate**, and the
core reads *past* the stored one at the provider boundary (`provider/usage.rs`: *"A stored `total_tokens` is
read past — the record derives"*, with a test asserting `total_tokens() == 12` "the sum, not the stored
total"). The hand-sync is two identical copies (`harness/snapshot.rs:360-366`,
`harness/forwarding.rs:210-215`), each writing
`cached_prompt_tokens: u.cached_input_tokens.unwrap_or(0)` — **erasing a fact the sibling type names**:
*"None is the provider not saying, which is not the same fact as a reported zero"* (`payload.rs:311-318`).
Reads of the stored sum: **exactly one**, `FleetList.svelte:48` (verified); `StatusBar` uses
`input_tokens`/`cached_prompt_tokens` and ignores it. **Shift:** `total` becomes a method,
`cached_prompt_tokens: Option<u64>` (restoring "not saying"), `#[serde(default)]` comes off so a partial
object is a decode error, not a zero. **Net −4** — taken for the invariant, stated plainly.
Red: `events.rs:266-270`, `snapshot.rs:414` compile-red. Negative to pin: `from_value::<Usage>({"input_tokens":3,"output_tokens":4})`
must be `Err`; today it is `Ok(Usage{3,4,0,0})`. No-op proof: the field's single prod reader has **no test**.

## 6L · `saw_interrupted` — a dead flag, with the ledger naming the fix round that orphaned it — **A**
`tau-acp/src/sessions.rs:38`. All sites, exhaustive: written at `pump.rs:119`, reset at `pump.rs:243` and
`session_methods.rs:243`, initialized at `sessions.rs:38`, `session_methods.rs:113`, `pump.rs:304`.
**Reads: zero** (verified independently; the settle rule at `pump.rs:189-197` reads `cancel_requested` and
`saw_stream_end` only). The ledger: `39fbdaf` (#81) built the rule `Some(if s.saw_interrupted ||
s.cancel_active { Cancelled … })`; `66472b2` (#84) deleted the `cancel_active` snapshot and rewrote
`queue_settle` **without the read**, leaving the field, its write and its two resets.
The guard comment is what keeps it invisible:
`#[allow(clippy::struct_excessive_bools)] // one bit per turn-lifecycle state; merging is worse`
(sessions.rs:20) — **true** for the three live bits, and it also shelters the fourth.
**Shift:** delete. **Net −8** (5 prod + 1 test + the `#[allow]` and its comment, which stop firing at 3 bits).
**No red test — that is the finding.**

## 5G · The entry id at the wire boundary, priced by a second pass — convergent with 5C, and scoped smaller
An independent pass reached the same root (`session/mod.rs:323` + `file.rs:185` +
`om_integration/turn_end/window.rs:78` — a third site I had not counted) and declined to price it:
a newtype touches 11 `mint_id` consumers and every wire id, with no net-negative proposal. **Agreed** —
which is exactly the scoping 5C lands (om_integration seam only). Convergent, and the restraint is the point.

## 2F · `self_weak` at a **third** site — `Supervisor` — where the mutex itself is the cost
`subagent/mod.rs:78-82,200,202`, sole reader `reclaim.rs:137-140`. `Supervisor::new` is the only
constructor and returns `Arc<Self>`, so post-build `None` is unreachable; the `?` silently drops a re-adopt
of an on-disk child with **no diagnostic**. **Shift:** `new() -> (Arc<Self>, Weak<Self>)`, field becomes
`Weak<Self>` — and `Weak::upgrade` takes `&self`, so **the mutex disappears** from the resolve→re-adopt
path. **Net −5.** The instruction that generalizes my 2E: **delete the `Option`, do not copy the
`.expect`** — that trades a silent drop for a panic.

## Not proposals, flagged as defects
**`transport.rs:52-53`** — `let _ = writeln!(guard, "{line}")`, `let _ = guard.flush()`. Adjudicated against
the repo's own best-effort standard (`pin_store`, which *states* the tolerance and its consequence): this
file states none. Its header says every log line goes to stderr; `send`'s doc says "write one message"; and
the same file carries an `expect` with a stated invariant at `:49` — so the crate does the "this cannot
fail" reasoning, and pointedly does not do it for the one operation that can. `Out` wraps
`Box<dyn Write + Send>` = `stdout` (`lib.rs:29`); a closed client pipe returns `EPIPE`, and then every
frame is discarded while `pump::run` keeps consuming events and `settle_headless` keeps nudging — **a
headless run spends provider tokens writing to a dead pipe indefinitely**, and the in-flight
`session/prompt` response is lost without trace. **Honest pricing: ~+10 LOC. This is the case where "add a
check" is the fix** — the invariant is not unenforced upstream, the failure simply is not handled. It fails
the net-negative bar, so it is a **ticket, not a refactor**: `Out::send -> io::Result<()>` propagated at 26
sites, with the stdin loop and `pump::run` treating `Err` as "the client is gone." Red test: none exists;
`transport.rs`'s tests never construct a writer that errors.

## Pushback I accept
- **`delta_seq` / the mapper↔sink counter split is NOT a finding, and my brief's lead L2 was wrong.** The
  producer marks in **bytes** (`sink.rs:31,34,38`, the delta is a byte-slice, char-boundary-safe because the
  mark is always a prior `len()` of the same accumulator); the consumer counts **chars** (`mapper.rs:15,16`),
  and the units *must* differ. Pinned from both sides: `sink.rs:470`, `mapper.rs:295`. The pump holds **no
  char state** — `pump.rs:45` is a doc comment; `delta_chunks` delegates to `mapper.on_delta`. One owner.
  Residue: `in_flight_set` is an O(1) index over a deque of ~2 → **−3**, no red test, `mapper.rs:379` is the
  no-op proof. Low value, reported as the honest floor.
- **`QueuedItem.id` is a structural win, not a finding** — required (no `#[serde(default)]`), minted at the
  construction site (`agent/queue.rs:113`), with a negative test that states the rule: *"a queue item with
  no core-issued id is a data error: the GUI deletes by id, so a row it cannot address is worse than no
  queue (#346)"* (`snapshot.rs:428-431`). This is the pattern findings 4A/4E/6K lack.
- `StreamOutcome` (closed 6-arm, no `Other`), `SubagentInfo.created`'s forward-compat (tested, with a
  `child` tie-break), `EntryPayload::from_entry`'s `unwrap_or_else(Other)` (ADR-0005 free-form-at-rest,
  positive+negative), `TurnUsage::from_record` (frozen legacy read, 4 tests), and every external-world guard
  in `config.rs`/`headless.rs`/`server/mod.rs` — all cleared, with citations in the agent's report.
- **`session/mod.rs:226`'s `let _ = self.ensure_open()` is load-bearing, and my 3B was over-stated.** The
  agent's reading is right that it is a **deliberate deferral with a stated reason**, not a blind swallow:
  `append_line` runs `ensure_open()?` first (`file.rs:135`), so the append path surfaces the same error.
  Where 3B survives: the promise *"surfaces on the following append"* is **not honored on the queue path**
  — verified at `agent/queue.rs:97-98`, `mint_id()` → `upsert_event(&id, …)`, which is `&self`
  (`file.rs:286`), fires the wire hook with no open and no validation, and keeps the id for the #148
  dequeue rewrite. So: severity **B, not A**; the `-> Result` shift stands (it is net-negative and makes the
  promise structural rather than a comment), and the honest framing is *"one path breaks the deferral's
  contract"*, not *"the regression is patched, not fixed."*

## Module verdict
**Structurally shifted, with one migration that stopped half-way.** #346 stream B (`e003ace`, "type the entry
payload on the wire") moved the payload to a tagged enum **and left every prod consumer reading the
flattened `Value`** — the type shift landed on the wire, not at the reader, and 4E is what grew in the gap.
The wins name the pattern the findings lack: `QueuedItem.id`, `SubagentState`'s tagged enum with an
`Other`-by-name escape, `Event`'s self-healing full-state arms, `ToolResult::status()` as the single verdict
producer, `TurnUsage` deriving its total and citing ADR-0034, `payload_value()`'s doc naming its own
consumers, and the #356 fix (*"a layer that will not load is the answer, not the system layer behind it"*)
with a test proving the old fallback was the compensation.


---

# Verdict

**Structurally shifted, with a real layer of accumulated patchwork — and the patches cluster at seams, not
in modules.**

The ledger is the evidence. fix:feat **3.4:1** crate-wide. The config arc: 7 commits, **+203/−76** on
`state.rs` alone. `Transcript.svelte`: 63 commits, net **+475**. When the same fact is defended at ten sites,
the fact is not in the type.

The counter-examples prove the fix is available: the virtua rewrite **+108/−638**; `ObservedSpan`;
`OmStage` + `OmError::Provider(stage, …)` (stages added without touching the variants); `TurnEndAction`
(the promoted set as a **return value**, the idiom 6E adopts); ADR-0008's single id-keyed entry map, which is
why `Transcript.svelte` contains **no reconciliation pass** over the entry list; `rules/*.ts` as pure
state→value functions with the store as the sole mutation site (294 lines of policy outside reactivity);
`subagent-state.ts` as the one narrowing point; `provider/overflow.rs` as the correct adapter for genuinely
unstructured input.

**The convergence is the confidence signal.** `config_stale` was independently found by two agents at the
same site (1B) with two compatible fixes. The `Registry` 5-map sweep (1C) is the same shape as
`close_workspace`'s 8-map sweep (1A) and `Mapper`'s 5-collection sweep (1H) — the same defect at three
unrelated scales. `stop`-flag duplication surfaced from three directions (2B's bug, 2C's pairing, HF7's
field) and is one root. `self_weak`'s unreachable `Option` appears at **three** sites (2E, 2F, and
harness/sessions.rs:283 where the code already writes `.expect("session_new on a built core")`). The subagent
record's hand-decode was reached by two passes at the same root. The entry id was reached by two passes that
*independently declined* the crate-wide scope — the restraint, not the finding, is the agreement.
**Every scope's deepest finding is one rule: state must travel with the thing it describes.**

**Deepest change available (Rust, structural):** the per-workspace aggregate (1A) — root of 1B, 1G, 1E, 6F
and the leak, and it pulls `state.rs` (893 LOC, the most ticket-cited file) off the wall.
**Deepest change available (Rust, defect-bearing):** finish the #346 payload migration at the *readers*
(4A+4E, 4C). The type landed on the wire and never moved the consumers; every gap that opened is where a
hand-decoder, an `unwrap_or("")`, or the 4E latch grew. Highest value per unit risk, because it deletes bug
*classes* rather than instances.
**Deepest change available (frontend):** the same discipline, one line — key state by the identity it
describes, let reads derive the rest (FE1/2/4/6, −50, and it deletes the codebase's only three
rule-violating state-synchronising `$effect`s).
**Corrections I made to agent proposals** (the bar is evidence, not volume): 5F's lifecycle enum is **wrong**
(three lifetimes, verified); 3D's −21 is **−7** (the "duplicate" child loop is a legitimate pre/post
re-check after the quiesce window); 6C's protocol-field deletion is a **spec violation** (§186 mandates the
leaf pointer — the fix keeps the field, computed); the `Compaction` event is **spec-mandated** (§191) so the
finding is a GUI handler bug, not dead code; the store layer's zero `$derived` is **the language**
(`derived_invalid_export`), not laziness, and my initial hypothesis was refuted and re-verified.

**Net LOC, per-finding (the arithmetic, not the estimate):** summing every finding's stated price gives
**−521 (tier A, 24 findings) + −214 (tier B, 19) + −8 (tier C, 1) + −64 (frontend FE1–8) = −785 production
LOC**, with 5 findings left unpriced (2B, 3E, 5G, 6G — 5G declined as unpriceable, see below). By root:
R1 −214 + FE(R1) −48 · R2 −79 · R3 −106 · R4 −81 + FE(R4) −16 · R5 −64 · R6 −114 (+1J −38, 1K −10).
**Deliverable: −770 ± 20.** Three adjustments: 4A is priced −10 and marked "net-negative *with* 4E," whose
−44 shares its handlers (summing both over-counts ~10); 2A/2C are one ticket (−53 combined); test LOC moves
the **other** way in several items (1D deletes ~200 test lines, 1J adds ~17, 3G must add the pair it lacks).
**The defensible number is the tier-A set: −520.** Tier B is single-sourced (one pass, no convergence).
**Ten changes carry −379 of it** (1D 60, 3A 55, 2A+2C 53, 4E 44, 1J 38, 1C 33, 6B 28, 1A 25, 1H 25, 2E 18);
the rest is a −4/−5/−6 tail listed for completeness, not as a queue. An earlier draft of this verdict said
−905; it summed agent subtotals that counted cluster items twice. This is the corrected arithmetic. It strips ~105 lines from
`state.rs`, ~102 from `om_integration/mod.rs`, ~81 from `forwarding.rs`, ~36 from `tau-acp/mapper.rs`,
~64 from `subagent/restore.rs`. Nothing here is a file split, and every item is net-negative. **No file
crosses below the 500-LOC soft limit from this set** — `state.rs` 893 → ~788, `hashline/mod.rs` 971 → ~935.
The soft limit is not the argument; the deleted logic is.

**Seven are not refactors — they are live defects the additive patches currently hide:**

| | defect | evidence |
|---|---|---|
| **4E** | A streaming `bash` tool reaches an ACP client as `completed` with **its first output chunk**; a **failing** command is reported `completed` | **reproduced against the shipped binary**; `acp_protocol.rs:700-703` asserts it as intended |
| **2B** | a stopped child sub-agent's call is recorded as a **clean completion** | the provider stop flag is an orphan in the production child path; tests take the wired fallback |
| **6A** | a storage failure becomes an **empty delta** — the turn's `StreamEnd`/tool/compaction events vanish | two `unwrap_or_default()` on a read that can fail after `open()` succeeded |
| **1J** | a gate-gap diagnostic is **suppressed forever** when its append fails | latch set `gate.rs:245`, fallible append `:254`, no rollback (the other three sites roll back) |
| **1K** | `handle_of` reads map keys **no writer in the repository writes**; every reclaimed child is renamed to its session id | grep: `"handle":` appears only as the reader; the test that names it asserts the fallback |
| **4C** | a fork child of any parent that has ever spawned a sub-agent inherits **a sibling's provenance** | `restore.rs:168` searches `head` first; the fixture parent has zero spawn records |
| **3G** | a failed state append leaves a child's memory and its record **permanently split**, with three divergent caller policies | zero tests touch the failure path; each policy arm is a measured no-op |

Plus one defect that is **not** a refactor and **not** net-negative: `tau-acp/transport.rs:52-53` swallows
`EPIPE`, so a headless run spends provider tokens writing to a dead pipe indefinitely. Its fix is ~+10 LOC —
the honest counter-example to a net-deletion rule, flagged as a ticket.

---

1. **The seven defects first (A).** 4E (client-visible, has a pin to delete), 2A+2B+2C (one ticket), 6A,
   1J, 1K, 4C, 3G. Six of the seven need no state model; 2B needs 2A's notice. `transport.rs` is its own ticket.
2. **Root 1 aggregate (1A)** — widest blast radius; 1B/1E/1G/6F are cheapest *after* it.
3. **The OM cluster (1D, 5A, 5B, 6D, 6E, 5E, 3C, 5C/5D)** — one seam, ≈−140; 5C/5D land together behind
   the disk-compat serde boundary (`OmRecord` has no version field, only `#[serde(default)]` tolerance).
4. **Finish the #346 migration (4A+4E together, then 4B, 4C/4E-typed-payload, 4D)** — the payload type
   landed on the wire and never reached the readers; that gap is where 4E grew.
5. **Frontend (FE1/2/4/6 as one −50 change; FE3/5/7/8 separately).**
6. **3A (−55)** — the largest single deletion; after 3B, which removes one of its callers' hazards.

## What I got wrong, and what the agents' pushback bought

Recorded because the audit's value is its calibration, not its volume.

- **My `$derived` hypothesis was false.** The store layer's zero-`$derived` is the *language*
  (`derived_invalid_export`, verified by compiling the module), not laziness. Refuted by the frontend agent,
  re-verified by me.
- **My brief's L2 lead (mapper↔pump char-counter duplication) was false.** The producer marks in bytes, the
  consumer counts chars, the units *must* differ, and the pump holds no char state at all. Real residue: −3.
- **My 4A named the right site with the wrong mechanism** — the shape-sniff decides *phase*, not rendering.
  Correcting it exposed 4E, the report's best finding.
- **My 3B was over-stated** ("the regression is patched, not fixed"): the deferral is deliberate and honored
  on the append path. Severity A → B; the `-> Result` shift stands on the queue path, which does break it.
- **3D's −21 is −7** (a pre/post re-check pair, not a duplicate loop) and **5F's lifecycle enum is wrong** —
  refuted by two independent passes enumerating the same flag lifetimes.
- Convergence is the confidence signal: `config_stale` (two agents, same site), the 5-map/8-map/7-map
  side-table sweep (three sites, three scales), `self_weak` (three sites), the subagent record (two passes,
  the same root), the entry id (two passes, independently declining the crate-wide scope). Two agents named
  `TurnSettleGuard` and the archive's flag-before-move as the wins to imitate.

---

# Addendum 4 — `hashline/` + `session/` (agent colorful-stocking), folded in 2026-11-03

Wave 4, pass 1 of 3. Scope: `hashline/` 3,695 prod LOC + `session/` 1,412. The agent's report ran the
suite green (`127 passed`, `git status` clean) and reported **11 findings / −105**. Every claim below was
re-verified against the source by me; **F11's shift is refuted** (its premise is false), **F7's byte
arithmetic is overstated ~24×**, and the agent's own totals don't sum (its items add to −157, its stated
total is −105). Verified net for this pass: **−138**.

The prior verdict "hashline is clean — pure functions, bounded pool, counted reclaim" **does not survive
a line-by-line pass.** It was correct about `lcs.rs`, `pin_store/`, `encoding.rs`, `line_endings.rs` and
`diagnostics.rs` (all confirmed clean below) and wrong about `registry.rs` and `mod.rs`.

## H1 · The write path keys the registry by a *different string* than read/edit — **A** (−3)

The registry key is the resolved absolute path: `read` resolves at `impls.rs:36` and keys
`path.display().to_string()` at `:116`; `edit` resolves at `:463` and keys at `:464`. **`free` is called
with the raw model argument**: `surface.rs:118` does `reg.free(path)` on `call.args["path"]` — unresolved,
relative, straight from the model. `Registry::free(&mut self, path: &str)` (`registry.rs:313`) forwards to
`evict_path` and to `store.load(path)` / `store.retire(path)` on that same raw string.

So `read("a.rs")` stores under `/abs/cwd/a.rs`; `write("a.rs")` frees `"a.rs"` — a guaranteed cache miss.
The §5.4 guarantee "a write voids every anchor of that file" is **broken for every relative-path write**,
which is the normal case for a model. Stale anchors stay in `owned`/`served`; the next `read` re-anchors by
LCS against the old file and re-mints; a subsequent `edit` plans against the pre-write baseline.

**Mitigation I must state, because it bounds the severity:** `edit` re-reads the file at commit
(`impls.rs:464-481`, the #207 §6 "read twice, abort on change" pair) and gates on
`reg.served_map(&path_str)` / `reg.anchors_for(&path_str)` (`:493-500`) with the **resolved** key. So the
hole is a broken invalidation guarantee and wasted anchor churn, not silent corruption of a write.

**Missing invariant:** "one path, one registry key" — four entry points, one construction. This is
root cause 5 (typed data round-tripped through strings): a `PathBuf` and a model `String` are the same
argument at three sites and one is normalized, one is not.
**Shift:** one `reg_key(cwd, args) -> String` at the `surface.rs` dispatch point, used by read/edit/write;
delete the raw-arg `free`. **Net −3** (−6/+3) — the price is irrelevant, the guarantee is the point.
**Red test: none.** `registry.rs:665` `free_drops_ownership_and_served` calls `free()` with the key it
allocated with — the string mismatch is unreachable in the unit test, so it stays green. **No-op proof: the
"write drops the baseline" contract has no integration test.** Pair required: positive = relative
`read` → `write` → `edit` on a stale anchor → `NotOwned`; negative = the same edit without the write applies.

## H2 · `line_endings: Vec<Ending>` is dead production output on every CRLF edit — **A** (−31)

`Planned.line_endings` (`mod.rs:403`) and `Edited.line_endings` (`:438`) are a per-line vector built at
`:903-912` — `vec![file_ending; lines.len()]` plus a `map_endings` allocation **per edited range** —
copied at `:950`, and read by exactly **one** consumer: `assemble` (`:964-970`) takes `.first()` for the
whole file, with `unwrap_or(Ending::Lf)` at `:969` for the empty case. `apply_edits_opts` **re-detects**
(`:940`) and joins with that (`:943`), never with `planned.line_endings`. Verified: `.line_endings` has
exactly two touches in production, `:950` write and `:966` read.

The type fixes the fact: `Ending` is a **scalar** — `detect` returns one `Ending`, and the module's own doc
says "dominant in the file, **one per file**" (`line_endings.rs:9,30`). The `Vec` models a per-line ending
this module never produces. `map_endings` (~19 LOC with doc) is dead with it.
**Shift:** `Planned.ending: Ending` built once from `file_ending`; delete the per-range loop, the per-line
allocation, the field on the public `Edited`, and the `unwrap_or(Lf)`. **Net −31** (−31/+3), up to −38 with
`map_endings`.
**Severity A, with the waste stated correctly:** an allocation per edit proportional to line count, discarded.
The agent's "240 KB per 10k-line edit" is **wrong** — `Ending` is a fieldless 3-variant enum, 1 byte, so a
10k-line file allocates ~10 KB and N per-range copies, not 240 KB. Real waste, 24× less than claimed.
**Red test: none.** `tools/tests_edit.rs:33` `crlf_files_preserve_their_line_endings` asserts the file
keeps its CRLF and stays green under the scalar. **No-op proof.**

## H3 · `resolve_anchor` decodes an anchor, discards the value, then linear-scans — **B** (−31 with H4)

`mod.rs:672-674`: `let Some(_idx) = hash_to_idx(anchor) else { return Err(BadAnchor) }` — the decoded index
is **thrown away**, then `:676` does `hashes.iter().position(|h| h == anchor)`. The same decode-then-scan
repeats at `registry.rs:461` (`resolve_offset`), `:524,:527` (`edit_target_error`), `mod.rs:587,:598`
(`strip_anchored_content`) — **5 sites**.
**Missing invariant:** "4 letters from ALPHA" is a type. **Shift:** `struct Anchor(usize)` parsed once at
`impls.rs:443` (`parse_edit_args`), carried in `Edit`; the scan becomes an index. **Net −26** (+14).
**Red, verified:** `registry.rs:927` and `tests.rs:134` both go red. Positive/negative pair exists at
`registry.rs:927`.

## H4 · Case-folded anchor hints scan the whole owned set — **C** (in H3)

`registry.rs:493-506` does `to_lowercase()` per owned anchor per miss, called at `:467` and `:587`.
With H3's `Anchor(usize)` this is a bounded numeric neighborhood. The agent priced −15; **counted inside
H3** (the type and the additions are shared) — H3+H4 as one change = **−31**.

## H5 · `registry.rs` is 7 hand-joined side tables; the LRU clock is derived state — **B** (−14 on top of 1C)

Extends 1C (do not re-price 1C's −33). Beyond the 5 maps 1C prices: `usage_clock` (`:52`) + `last_used`
(`:51`) + `touch` (`:109`) + `reclaim`'s `min_by_key` (`:407`) **are** an LRU, stored as a counter and
recomputed by a linear scan. A `VecDeque` MRU in the per-path entry deletes the counter, its increment, and
the scan. **Net −14, +0.** **Red test: none** — grep for `reclaim|last_used|usage_clock` in
`tests*.rs` returns nothing. **The LRU eviction path is untested**; no-op proof.

## H6 · The mint probe cursor is per-file state in a global table — **B** (−6)

`probe` (`:55`) + `file_hint` (`:60`), written at `:366`, read at `:349/:354` with **two**
`unwrap_or(self.probe)` fallbacks, dropped at `:422`. Fold the hint into the per-path entry: one field, one
fallback. **Net −6, +0. Red, verified:** `registry.rs:757` asserts `reg.probe` — goes red.

## H7 · `mint` walks the 7.3M-slot space twice and re-types a named constant 3× — **B** (−5)

`registry.rs:357` `for step in 0..=crate::hashline::HASH_SPACE` (off-by-one over-count) inside the outer
`loop` at `:356` with the reclaim retry at `:371` → **two full-space walks (~14.6M iterations)** on the
pool-full path. `HASH_SPACE - 3` is written inline at `:358`, `:363`, `:364` while **`const STRIDE`
exists** (`mod.rs:43`, used correctly at `:337`, `:358`, `:362` — the same file, three spellings).
**Shift:** use `STRIDE`, `0..HASH_SPACE`. **Net −5** (+2). Not a correctness bug: a CPU stall on an
untested path (`tests.rs:199` stops short of the cap) — no-op proof.

## H8 · `lcs_reanchor` truncates two ledgers that drift — **B** (−20; the guard is load-bearing)

`mod.rs:624` `.take(old_hashes.len())` with an 11-line comment, plus `.expect("all lines anchored")` at
`:665` for the same ledger. **This guard masks a proven production panic:** the red test's own header
(`tests.rs:540-543`) records `index out of bounds: the len is 299 but the index is 299` from a
tokio-rt-worker. Deleting `.take()` reproduces it — **verified load-bearing**, the opposite of a no-op.
**Root:** the caller passes `(lines, hashes)` that are supposed to be in step and the type cannot enforce it.
**Shift (the principled one):** pass `&[(line, anchor)]` — one collection, one length, impossible to desync.
**Net −12** for that step; **−20** for the drift-proof key in H9, which **subsumes** it (take −20, not −32).

## H9 · The registry serves anchors for a file it never re-checks — **B** (−20, subsumes H8)

`anchors_for` (`registry.rs:236`) returns anchors for a path with no freshness signal; `session/` re-stats
its log (`refresh_if_grown`), hashline does not. Every edit plan runs against a snapshot of arbitrary age,
which is what makes H8's drift reachable at all (external writers, FTS, agents editing outside the tools).
**Shift:** key on `(path, len, mtime)` captured at `allocate`, checked in `anchors_for` → `None` on drift;
makes the illegal state unconstructable rather than truncated. Deletes H8's comment block and
`plan_edits_registry_shorter_than_file_does_not_panic` (43 LOC) as unreachable.
**Red, verified:** `tests.rs:544` **and** `tests.rs:588` — both assert the illegal state is tolerated, so
both move up to `allocate`-level as drift tests.

## H10 · `SessionStore` mirrors the file and guards `loaded` at 7 read sites — **B** (−28)

`entries`/`entry_len`/`file_len` mirror the file; `file_len` is hand-resynced after every append
(`file.rs:174-176`, comment: "keep the bookkeeping `refresh_if_grown` relies on"). Seven guards of the shape
`if !self.loaded { return self.<file-backed twin>(); }` at `log.rs:32, 58, 83, 105, 139, 153, 167`, plus the
`!self.loaded || len > self.file_len` at `:19`.
**Missing invariant:** the mirror is valid only at a known file length. **Shift:** `open` returns
`Loaded { at_len, entries }`, so a cached read requires the proof; no `loaded` flag. **Net −28** (+5).
**Red test: none** (`tests_log.rs:55` tests the length check only). No-op proof.

## H11 · `Registry::freed` grows with no drop path — **refuted as priced; B as a note** (−0)

The agent claimed −9 by making the mint's `taken` closure (`registry.rs:343`) consult
`owned ∪ store.retired`. **The shift is not available.** Verified: there is no store-level retired-set
query. `retired_anchors()` is a method on `FilePins` (`record.rs:94`) — per-record, called from
`record_tests.rs`/`pin_store/tests.rs` only — and `pin_store/mod.rs` exposes `load(path) -> Option<FilePins>`
(`:331`) and `retire(path)`, not a set. `freed` is the session-invariant claim set
(`record.rs:296-299` says dropped claims are legal in a *later* session; `freed` is what makes them
invariant *this* session). Deleting it breaks a real contract. **−9 → 0.** The observation stands
(unbounded per-session growth, comment at `:63-68` admits "it only grows"), the deletion does not.

## Files adjudicated clean (verified by the agent's line-by-line pass; I spot-checked)

`hashline/lcs.rs` (292, pure, Hirschberg, proptest vs brute-force oracle); `hashline/pin_store/record.rs`
(211, `MAX_TOMBSTONES` = documented counted cap); `hashline/pin_store/mod.rs` (432, documented best-effort
SQLite degradation, quarantine, batched GC); `hashline/encoding.rs`; `line_endings.rs` (module logic —
`detect` is one scalar, the debt is the `Vec` misuse in H2); `hashline/diagnostics.rs` (pure formatting);
`session/mod.rs` `open`/`entries_*`/`Header` (bounded reads, CRC verify, torn-tail **read-side** tolerance —
the writer settles the tail, that split is a principled invariant, not a repair); `session/entry.rs` (const
CRC table, typed serde); `session/list.rs`; `session/file.rs` `settle_tail`/`archive`/`read_blob`
(documented writer-side repair at a defined point); `registry.rs::persist_pins` (documented best-effort);
`apply_gc_policy` (edge-triggered reload via `last_policy: Mutex<GcPolicy>`, not a hand-synced mirror).

Not re-reported (adjacent, honored): 1C, 1F, 3B+5C `mint_id`, 5D, `persist_pins`.

## Converges with the report's roots

**H1** is root cause 5 (a path as a `String`, normalized at 3 sites and not at the 4th) and root cause 6
(the invalidation the design mandates does not fire). **H2** is root cause 5's mirror image: a scalar
modelled as a vector, read at one site through `.first()`. **H5/H6** are root cause 1 at the 7-map scale —
that is now the side-table sweep at **four** scales: 11 (Core), 8 (close_workspace), 7 (Mapper), 5–7
(Registry). **H8/H9** are root cause 3 (a repair pass truncating drift at the read site) and the third
instance of "the guard for a proven past panic". **H3** is root cause 5 again: a 4-letter address parsed
5× and its decoded value discarded.

**Honest headline for this pass: 5 of 9 priced findings are no-op proofs** — H1, H2, H5, H7, H10 have no
test that goes red when the guard is deleted. That includes both A findings and the two worst sites in
`hashline`. The tests do not cover the invalidation guarantee, the CRLF path's cost, the LRU eviction, the
pool-full mint path, or the cold-store read path. **That is a finding about the test suite, and it is the
most useful sentence in this addendum.**

---

# Addendum 5 — `tools/` + `tau-mock-llm/` + frontend components (agent outstanding-line), folded in 2026-11-03

Wave 4, pass 3 of 3. Scope: `tools/` 1,970 + `tau-mock-llm/` 680 + the frontend **component tree** 5,769.
The agent reported 19 findings / **−229** / 6 live defects. Every claim was re-verified against the source
by me. Net after adjudication: **−185**, because **R6's −44 is refuted** (its shift deletes a live feature)
and **two of the six live defects are refuted**. Tree verified clean by the agent; baseline green.

Three of its frontend findings are **re-prices of FE1/FE4/FE6** — same files, same sites, deeper shift.
I take the new (larger) prices and mark the originals superseded, so the report does not double-count;
the upgrades add **+32** to the wave-1–3 total (below).

## T1 · The tool arg schema is re-parsed by hand at eight entry points — **B** (−19)

`args.get(k).and_then(as_str)` + a missing-arg error, written at `impls.rs:36-38` (read), `:281-286`
(write, twice), `:409-437` (`parse_edit_args`), `:767-769` (bash), `:770-774` (`timeout_secs`),
`surface.rs:123-127` (edit `strict`), `:136-140` (bash `background`), `:141-147` (bash `command`
**a second time**). The schema already declares the required set — `mod.rs:131-236` carries
`required: ["path"]`, `["path","content"]`, `["path","edits"]` + `minItems: 1`, `["command"]` — and
**nothing reads it**: `grep -rn "\.schema" crates/tau-core/src` returns zero consumers. The declared
contract is prose shipped to the model; the enforced contract is eight hand-parses that can drift from it.
**Shift:** parse `call.args` **once** in `dispatch` (`surface.rs:95`) into one args type per tool; impls
take the parsed shape. **Net −19** (−46/+27).
**Red:** `tests.rs:191` `write_missing_args_is_an_error` — the **only** arg-shape test in the crate.
Read/edit/bash shapes, the `timeout_secs` default: **no-op proofs**.
**Defect I refuted:** the agent called `impls.rs:773` `.max(1)` a live defect that "overrides a
model-requested value". The schema declares `"timeout_secs": {"type": "integer", "minimum": 1}`
(`mod.rs:202`), so `0` is out-of-contract input and clamping **honors the declared bound**. Not a defect.
The finding is the duplication, not the clamp — and the clamp is itself root cause 5's shape: a declared
bound with no type to carry it.

## T2 · `ToolResult`'s verdict is two `pub bool`s with an illegal corner — **B** (−6)

Four constructors (`mod.rs:46-90`) define the three legal `ok`/`cancelled` pairs; the fields are public,
so the fourth pair is constructible — and `impls.rs:853-858` builds the struct **as a literal**, bypassing
the constructors that exist to fix it. A 3-state fact stored as a 2-bit product.
**Shift:** private fields + `ok()`/`cancelled()`/`status()`. **Net −6** (−16/+10). **No-op proof** (no
test touches the illegal pair).

## T3 · The background bash branch re-reads `command` and defaults it to `""` — **B** (−5)

`surface.rs:141-147` parses `command` a **second** time with `.unwrap_or("")`, duplicating the real parse
at `impls.rs:767-769` (which errors). `{"background": true}` with no `command` spawns `bash -c ''` — a job
that exits 0 and is **reported as started**. Verified: the foreground path errors, the background path
invents an empty command. **Shift:** hoist the parse above the background/foreground split. **Net −5.**
**No-op proof** — `agent/tests_bgjobs.rs:47` and `tools/tests_bgjobs.rs:14` always pass a command.

## T4 · `Some(_) => "bash: internal error: unknown outcome"` — **C** (−2)

`impls.rs:842-847`: an arm for a value the code above cannot produce, living because `outcome` is
`Option<&str>` not an enum. `enum Outcome { Stop, Timeout }` makes it unconstructible. **Net −2.** No-op.

## T5 · `report_detail` — the audit's largest claimed prize, **refuted as a shift** (−44 → 0)

The agent priced −44 to delete `report_detail` (`progress.rs:119-158`), calling it "dead in production"
with a live defect: it emits `output: ToolOutput::Text(String::new())` (`:139`) and so "blanks the tool
card on the 16 ms cadence". **Both halves are wrong.**
- The empty `output` is the **stated contract**: the doc comment at `progress.rs:116-118` — "the `ui_detail`
  twin of `report` (#243): the final result wins, and the detail never rides `output`."
- The frontend reads detail **first**: `presentation.ts:99` — `const body = detail ?? e.output ?? ''`, with
  the comment "a split tool (write/edit/read) carries the card's content in `ui_detail` — the model only
  sees the ack; the card renders the detail, `output` is the fallback."
So the card is never blank: `ui_detail` carries the growing content and wins the `??`. And `report_detail`
is **not dead** — its one caller is `impls.rs:300` (`write`), reached through the production `progress`
constructor at `agent/turn/tools.rs:145`; it is the mechanism that renders a `write`'s content on the card.
The proposed shift (stream it through `report`) puts the file's content in `output` — which is what the
**model** sees. That is a #243 regression: the model would receive file bodies it is contractually shielded
from. **−44 → 0.** The split is deliberate design, adjudicated as legitimate external-contract handling.

## T6 · `image_max_pixels` is `Option` all the way down, defaulted at the leaf — **B, hole (net 0)**

`impls.rs:250`: `max_pixels.unwrap_or(crate::config::ImageLimits::default().max_pixels)`. The only
production producer sends `Some(config.limits.image.max_pixels)` (`harness/sessions.rs:580-581`), so the
`Option` is decorative — and `TurnConfig::default()` (`agent/mod.rs:187`) carries `None`, so an
agent-test-kit session silently enforces the built-in 2,097,152 px instead of the configured budget.
`ImageLimits` is `#[serde(default)]`, so the leaf default **is** the config default: the `Option` buys
nothing and launders a config value through a literal. **Shift:** `image_max_pixels: u64` on `DispatchCtx`
and `TurnConfig`. **Net 0** (−5/+5) — reported as a **hole**, correctly, by the agent's own discipline.
**Red:** `tests_image.rs:126` passes `None` and asserts the config default — the test *depends* on the hole.

## T7 · Three image config keys are parsed, advertised, and **never honored** — **A, product defect** (unpriced)

Verified by grep: `jpeg_quality` (`config/sections.rs:429`, default 80), `max_width` (`:423`, 2048),
`max_height` (`:424`, 2048) have **zero reads outside `config/` and tests**. `read` hard-codes quality —
`encode_jpeg_q85` (`impls.rs:275`, called at `:66`, comment `#252` "normalize to a canonical JPEG q85 wire
form") — and ignores width/height entirely. A user who sets `jpeg_quality = 60` gets q85.
The test that "covers" this asserts parsing only: `config/tests/model_facts.rs:205` sets `max_width = 1024`,
`layers.rs:15` asserts `jpeg_quality == 80` **as a parse result**. **No-op proof, and the worst kind**: a
green test that looks like coverage of a feature that does not exist.
**Not a net deletion.** Honoring the keys is an addition; deleting them is a product decision (the config
surface advertises them). Flagged for a ticket; the honest price is "one of these two, decided by a human".

## M1 · The mock's `response.completed` frame omits the fields the real provider completes with — **B, hole**

`scenario.rs:341-350`: `response.completed` carries `{id, usage}` only — no `output`, no `status`, no
`model`. The crate's stated contract (`lib.rs:1-5`) is "serves the Responses API SSE dialect that
tau-core's provider decodes". A fixture hand-builds the `json!`, so "what the endpoint sends" is a
convention shared between provider code and fixture code, with nothing enforcing it. The mock **lies by
omission**: core's decode path for response-complete is exercised against a thinner shape than production.
**Net addition** (render the completed object) → reported as **incoherence, not a shift**. Severity B:
this is the mock-LLM's whole purpose, and the determinism contract (ADR-0010) makes it worth a ticket.

## M3 · `capture_request` swallows every capture failure — **C** (−2)

`scenario.rs:253-267`: `let _ = create_dir_all`, `serde_json::to_string(req).unwrap_or_default()`,
`if let Ok(mut f)`. A failed capture yields a **missing** `requests.jsonl`, and `accept-sender-labels`
fails on a missing file far from the cause — in infrastructure whose entire job is to be a trustworthy
witness. **Shift:** resolve `capture_dir` at the builder, return `Result`, `tracing::warn!` on failure.
**Net −2.** No-op proof.

## FE9 · `EntryCard`'s reasoning slot — **A** (−21; **supersedes FE1's −14**)

The FE1 site, deeper: `EntryCard.svelte:42-47` seeds a local `open` object from `store.entryOpen`;
`:49-55` `setOpen` **dual-writes** (store map **and** local, verified at `:52-53`); `:58` `thinkSynced`
is the flag; `:60-66` the `$effect` synchronises `store.reasoningOpen` into `open.think`. **AGENTS.md
"Svelte", first rule** — an effect whose body is pure state synchronisation.
**Shift:** delete `thinkSynced` + the effect; visibility as `$derived`; key the per-entry override as a
**tri-state** (key present = explicit, absent = follow global) so `setOpen` writes one map; make
`entryOpen` a reactive `$state(new Map())` and delete the "Maps are not reactive" comment block at `:39-40`.
**Net −21** (−30/+9), up from FE1's −14. **Red, verified:** `EntryCard.test.ts:79` and `:95`.
**Defect I refuted:** the agent called the global-toggle-becomes-a-no-op-for-a-clicked-entry a live defect.
The code's own comment (`:56-59`) states it as the product rule: "a click stores a per-entry override that
then wins: the effect syncs only entries without an override, so a clicked line keeps its state across
global flips." That is documented intent — a product question at most, not a defect. Severity A stands on
the rule violation and the cluster root, not on the interaction.

## FE9b · Three components keep an async mirror + a staleness flag — **B** (−60)

The largest frontend finding of the audit, and a genuine cluster: `ChangesView.svelte:26`
`lines = $state([])` + the effect at `:31-59` with a `live` flag; **`ToolOutput.svelte:28-32`**
(`header`/`truncated`/`lines`) + `:34-63` (same shape, same flag); **`Markdown.svelte:13-14`**
(`html` + `gen`) + `:16-24`. All three re-implement the same async-derived-content guard. The fix already
exists in the codebase: `ModelMenu.svelte:21` reads store-owned state with **no** mirror.
Verified live defect: neither async effect clears `lines`, so a changed `text` leaves the **previous file's**
highlighted lines on screen for the whole re-highlight round trip.
**Shift:** one `asyncContent(source, lang, kind)` store module owning `{lines, header, truncated, error,
loading}`; the three become reads, plus one shared loading arm ("loading" is currently spelled three ways:
`next === null`, `providers === null`, `lines.length === 0`).
**Net −60** (−100/+40). **Red:** `ToolOutput.test.ts:51`, `ChangesView.test.ts`, `Markdown.test.ts` —
the trio is tested **three times against three implementations**: the duplication, not the gap.

## FE9c · `pane()` / `ensurePane()` — **B** (−30; **supersedes FE4's −10**)

FE4's shape counted across the tree: 19 `pane(ws)` call sites carry a null arm (`LeftPane.svelte:44, 48,
55, 112, 119, 136, 169, 174, 178, 207, 221, 231, 236`; `RightPane.svelte:64, 68`;
`SessionNode.svelte:55, 63, 73, 84, 96, 102, 129, 151, 169, 199, 212, 260, 279`). The split is
`store/state.svelte:160-173`; the "remember to call it" sites are `LeftPane.svelte:19-21` and
`RightPane.svelte:22-24` — **the two verbatim rule-violating `$effect`s FE4 found.**
**Shift:** `openWorkspace` (`store/workspace.svelte.ts:73+`) calls `ensurePane(ws.id)` — the single place a
workspace becomes live; every pane read becomes an unconditional local. **Net −30** (−35/+5).
**Red, verified:** `LeftPane.test.ts:71`, `RightPane.test.ts:65`, whose comments name the effect.

## FE9d · `Transcript`'s new-activity counter is state sync in an `$effect` — **B** (−8)

`Transcript.svelte:119-121` state + `:124-135` an effect that writes `signalSession`, `newCount` (twice),
`lastSignal`. Root: the pill derives from a scroll position that is component-local
(`shouldStickToBottom` at `:28`, written from virtua's callback at `:166-170`).
**Shift:** move the stick flag to `PaneState` (the `selSub` shape); `newCount = $derived(...)`. **Net −8.**
**No-op proof** — `Transcript.test.ts` has no pill/newCount case.

## FE9e · `openedObj` shadow map — **B** (−12; **supersedes FE6's −7**)

`Transcript.svelte:176` `openedObj = new Map<string, unknown>()` (id → session object), `:197-201` the
identity comparison that drops `win.openedFor` and `win.lastFetched`, `:202` the set. Plus `:26`
`mountedFor` (a plain-const snapshot with a comment explaining why `$derived` was wrong) and `:126-128`
`signalSession`. State keyed by **id** in one place, by **object identity** in another, repaired by a
shadow map. **Shift:** an `opened` counter on the session record (`tailJump`'s existing shape,
`state.svelte:60`); all three mirrors go. **Net −12** (−18/+6). **Red, verified:**
`Transcript.test.ts:119` (names the re-open case), `:176`.

## FE10 · Small component debt — **B/C** (−32)

- **`SessionNode` + `WorkspaceTabs` keep the same menu anchor twice** — `SessionNode.svelte:196` (`ctx`) +
  `:199-202` + `:203-207` writes **both** + `:259`; `WorkspaceTabs.svelte:22, 25-28, 36-38, 48-53, 106`.
  The clamp is shared (`display/useContextMenu.svelte.ts:34-39`), the state shape is not. `useContextMenu`
  owns `{anchor, pos, el}`. **−8.** No-op proof (tests assert menu presence, never position).
- **`ModelMenu` re-implements the overlay** — `:16` writes global `store.modelMenuOpen`, `:33-45` its own
  scrim + key handling, against `AttentionSheet.svelte:16-23` / `CommandPalette.svelte:76-83` /
  `FleetList.svelte:34-41` which all render through `Overlay`. **−12.**
- **`RightPane` re-materializes an empty plan** — `:73` `cur?.plan ?? {steps: [], criteria: '', notes: ''}`
  on a field the wire type makes non-optional (`wire/protocol.ts:566-572`, `rules/sessions.ts:67-73`).
  **Net 0 — a hole**, correctly priced.
- **`store.tailJump` is a signal only an effect can consume** — `state.svelte:60`, `composer.svelte.ts:16`,
  consumed as `void store.tailJump` at `Transcript.svelte:250-253`. **Net +2 — a hole**, correctly refused.
- **`Composer.complete()`'s `m.skill!`** — `:113` asserts past `Suggestion.skill?` with a rule tested by id
  strings (`:96, :104`). A discriminated union is **+3 — a hole**, correctly refused.

## Holes reported as holes — the discipline this pass adds

R3 (−0), F4 (−0), F7 (+2), F9 (+3), M1 (addition), M2 (no deletion exists: the mock's turn cursor **is** its
purpose, and `/__control/reset` is the documented wdio-retry contract). Six findings the agent **declined to
price as deletions** because no net deletion exists. That is the rule working, and it is the first pass in
this audit to report holes at scale.

## Files adjudicated clean (verified)

`tools/read_page.rs` (the 1 MiB page cap and 100 KiB line placeholder **bound a channel** feeding model
context — the prompt's stated exception; the oversized-line `sed` hint is a recovery path);
`tools/bgjobs.rs` (the `live` flag crosses a task boundary — waiter task, session close, completion gate —
`kill_all`'s abort-and-clear is the only way to say "no longer live" there); `tools/edit_diff.rs` (200-line
diff cap bounds a GUI channel; `model_ack`'s arithmetic pinned by `tests_edit_ack.rs`); `tools/mod.rs`
(the constructors, `status()`, `from_result` are the boundary this audit asks for); `tools/surface.rs`
beyond T1/T3 (`unwrap_or(false)` at `:127`/`:140` are the schema's declared `"default": false`);
`impls.rs` beyond T1-T4 (`write_atomic_bytes_inner`, `bash_path`, `stdbuf_in_path`, `bash_argv`,
`downscale_to_budget`, `looks_like_image`, the `edit` drift re-read + ownership gate: filesystem,
environment and TOCTOU are not type-enforceable; each `expect` names its invariant — `:51, 549, 785, 804-805, 839`);
`tau-mock-llm`: `chunk_text` (round-trip, char-safe), `select` (fallback-not-error, tested),
`read_request`'s 10 MiB caps (real network), `WireRequest`'s lenient decode → fallback (designed, tested
`scenario.rs:398`), `main.rs`;
frontend: `App.svelte` (both effects are outside-world — `document.title`/`setTitle` `:30-40`, keydown
`:138-140`), `TreeNode.svelte`, `Overlay.svelte`, `CodeLines.svelte`, `FocusRails.svelte`, `Queue.svelte`,
`StatusBar.svelte`, `PlanView.svelte`, `SubagentBlock.svelte`, `FileNode.svelte` (its `?? null` at `:16`
indexes `FilesCache` — genuinely `undefined`; non-optional `entry.dir`/`entry.ignored` honored, no
re-guard); `Composer.svelte`'s scrollIntoView effect (`:62-64`) is DOM, legal;
`SessionNode.svelte:101-109` is the blessed DOM-after-`tick()` case, `renamingId` single-writer;
`Transcript.svelte`'s pin (`:144-155`), `ResizeObserver` (`:158-172`), one-shot open (`:193-205`) and
turn-end (`:217-221`) effects drive the third-party virtualizer and DOM.
And the `?? []` / `?? 0` / `?? ''` reads on `Record<string, X>` (`ChangesView:17`, `Composer:50`,
`ModelMenu:21`, `RightPane:48/57/60/157/168`, `FileNode:16`, `Transcript:58/63/110/185/341`,
`StatusBar:13-19`, `WorkspaceTabs:11`, `FleetList:30`) are **legitimate**: under
`noUncheckedIndexedAccess` an index is `X | undefined`, not a wire-type re-guard. This is the
discipline FE3 (`tau-acp`'s 32 re-parses) tests for, and the frontend **passes** it.

## This pass's test-suite finding

**10 of 19 findings are no-op proofs** — T2, T3, T4, M1, M3, FE9d, FE10's menu/overlay/holes, and (per
Addendum 4) H1, H2, H5, H7, H10. Concretely: Rust arg-parse coverage exists for `write` **only**
(`tests.rs:191`); `ToolProgress` has **zero** tests; `Transcript.test.ts` has no pill/tailJump case;
context-menu tests never assert position; three image config keys are "covered" by a parse assertion for a
feature that does not exist. Wave 4 has now produced **15 no-op proofs across two passes** — the
recurring result of reading code that has been patched rather than designed is that the patches are
untested, and the tests that exist pin the patch (FE9b's trio tested three times against three
implementations; T6's test *depends* on the hole it exposes).

---

# Addendum 6 — `provider/` + `config/` + `skills/` + `plan/` + top-level (agent weary-temper), folded in 2026-11-04

Wave 4, pass 2 — **the last scope**. 5,955 LOC, closing coverage to ~99.9% of production.
The agent reported 15 findings / −264 / 2 live defects, with a clean baseline (`--lib` over scope: 248 passed).
I re-verified every claim at source and re-measured its headline item: **B3's −268 is arithmetically
impossible as stated** (corrected −264, and its severity drops C). Its A1 is a **confirmed live defect**
whose fix is net-positive — reported by the agent as such, unprompted. Adjudicated: **−281**, 4 holes.

## B3 · The scripted-provider test seam ships in the production binary — **C** (−264, relocation)

`provider/canned.rs` is 372 LOC shipped behind `pub use canned::*` (`provider/mod.rs:516`).
**The agent's arithmetic is wrong and I corrected it:** `canned.rs:16-73` holds `TurnProvider` (the trait),
`TurnProviderRef`, `production()`, and `ProductionProvider` — the **live** provider surface, undeletable.
The deletable set is `canned.rs:75-372` (298) minus the resolver `canned_dev` (`:351-372`) = **~276**.
Verified: `canned_cut`, `canned_slow`, `prefill_hang`, `canned`, `canned_slow`/`SlowFirstProvider` have
**zero non-test callers** (`canned_dev` has exactly one, `harness/forwarding.rs:229`).
**Shift:** `#[cfg(test)] mod canned;` — tau-core's own tests keep the seams (`provider/tests/canned.rs`,
`harness/tests_stream.rs:376`, `tests_model_switch.rs:125`, `tests_child_route.rs:17`); the production
binary loses them. `decode_stream` (`stream.rs:217`) stays — production-surface callers
(`harness/testkit.rs:189,238`, `subagent/testkit.rs:118`).
**This is relocation, not deletion from the repo** — production LOC −276, test-surface LOC +276.
**Net −264** (−276 / +12 for a typed release refusal).
**Severity C, not B — the agent's defect claim refuted.** `forwarding.rs:222-223` documents the exact
behaviour: "A `canned://` entry in a release build has no scripts, so it falls through to the live
endpoint, where the unresolvable URL fails the turn." Deliberate, named, requires the user to hand-write a
debug-only scheme into `~/.config/tau/config.toml`. The real finding is the **doc contradiction**:
`canned.rs:347-349` says release "returns `None` and the caller **must refuse**"; `forwarding.rs:229`
refuses nothing. Two comments, one seam, opposite contracts.

## A1 · A blank model key spams the log forever; its guard's comment describes a mechanism that does not exist — **A, live defect** (net +3, **not a deletion**)

Confirmed by execution and by grep: `set_model`'s justification (`diagnostics.rs:142-144`) — "The
hand-written `Diagnostic` deserializer defaults a missing field to `""`" — is **false**.
`grep "impl.*Deserialize" crates/tau-protocol/src/diagnostic.rs` → **no matches**: derived
`#[serde(tag = "code")]`, required `to: String`. A truncated record decodes to
`Diagnostic::Unknown { unknown_code: "model_changed" }` (the agent ran it in `/tmp`) — so the typed arm at
`diagnostics.rs:109` **never sees** the case the comment cites. Its test,
`diagnostics.rs:207 a_truncated_model_record_answers_nothing`, stays **green with the guard deleted**:
a no-op proof of exactly the half the comment describes.
**The reachable defect (verified through every hop):** `models: BTreeMap<String, ModelDef>`
(`config/sections.rs:16`) makes `[providers.dev.models.""]` a legal key; nothing refuses it at load
(`spelling.rs:20-36` checks provider names and thinking keys, never model keys). It reaches
`default_model` (`harness/sessions.rs:383-390` → first key = `""`) → `session_open.rs:38-45` pushes
`ModelChanged { to: "" }` → `set_model` refuses → `facts.model` stays `None` → **every reopen appends
another `model_changed`** to the append-only log, and the snapshot reports no model while the agent runs `""`.
**Not a net deletion.** The fix is relocation: refuse the empty key at load, then delete the three hand-checks
(`diagnostics.rs:145`, `dispatch_sessions.rs:108-112`, and `sessions.rs:196`'s `trim()` convention).
**Net +3** (−13/+16). The agent reported it as non-deleting rather than inflating it — the correct call
under this brief, and the reason I kept it in at all. The guard is load-bearing today; the false comment
costs nothing to delete and misleads the next reader about what the guard is for.
**Pair the fix needs:** negative — `[providers.p.models.""]` fails `config::load` naming the path;
positive — a non-empty key appends `model_changed` **at most once** across reloads
(`harness/tests_om_route.rs:432` is the shape).

## B1 · `plan::with_path` hand-computes byte offsets into a section this same file constructs — **B** (−5)

`plan/mod.rs:261-269`: `prompt.get(start..).and_then(...).unwrap_or(prompt.len())` for `header_end`, the
same shape for `end`, `prompt.get(..start).unwrap_or(prompt)`, `prompt.get(end+2..).unwrap_or("")` — four
bound folds because the section is located by **offset arithmetic**, over a shape that `section()`
(`:228-242`) and `PLAN_SECTION_ANCHOR` (`:221`) fix in this file (one paragraph after the header).
**Shift:** `split_once(PLAN_SECTION_ANCHOR)` → `head`/`rest`; `rest.split_once("\n\n")` → `(old, tail)`; the
offsets, the four folds, and the `+2` arithmetic go. The trim loop (`:270-272`) stays.
**Net −5.** **Red:** `plan/tests.rs:182`; pair `:191`, `:203` (the composite-anchor negative); keep
`plan/tests.rs:349` as the invariant pin. One production caller (`harness/launch.rs:298`).

## B2 · `skills` clones the line vector to re-parse it; `unquote` re-proves its own bounds — **B** (−3)

`skills/mod.rs:234` — `parse_fields(lines.clone(), false)` allocates a whole `Vec` clone to feed a second
pass, and `unquote` (`:253-261`) checks `v.len() >= 2`, then needs `expect("the quote check bounds the
slice")` at `:257` — the expect exists **only because** the bounds were computed by hand.
**Shift:** pass `&[&str]` to both passes (clone gone); `v.strip_prefix(q).and_then(|t| t.strip_suffix(q))`
makes the over-length case unnameable and the `expect` disappears with it. The strict-then-lenient two-pass
parse is the cross-client contract for foreign `SKILL.md` files — that stays. **Net −3.**
**Red:** `skills/tests.rs:120` (the quoted retry is load-bearing), `:222`. The `expect` is a **no-op proof**:
no test feeds `description: "` (one char, quoted). Third hand-rolled frontmatter reader in the crate:
`agent_type.rs:96-100`, with no retry — the divergence is the sibling symptom.

## C findings — 11, and six of them are honest holes

| # | Site | Verdict after my check | Net |
|---|---|---|---|
| **C1** | `config/sections.rs:150-158` `GenerationProfile::is_empty` | **Dead, verified**: the only `is_empty` on a profile in the workspace is `extra_body.is_empty()` *inside its own body* (`om_integration/mod.rs:84` is `extra_body`, not the profile). No callers anywhere, prod or test. | **−9** |
| **C3** | `provider/mod.rs:87-89` `if key_env.is_empty() { return None }` | **Redundant, verified by the agent's `/tmp` repro**: `env::var("") = Err(NotPresent)`, so the `var` path answers identically. The `.filter(|k| !k.is_empty())` at `:93` validates env-var *content* and stays. `provider/tests/models.rs:91` stays green → **no-op proof**. | **−3** |
| **C4** | `provider/request.rs:203-208` `.get(from..).unwrap_or_default()` | The offset crosses the crate seam as a bare `usize`, hand-latched by `agent/turn/mod.rs:289-292`; the fold is the seam tax. Thread a validated slice. Red: `provider/tests/wire.rs:315`, `:291`. | **−3** |
| **C5** | `agent/turn/mod.rs:376` `.budget().unwrap_or(0)` | The comment at `:371-375` declares the `Missing` arm unreachable here; the fold is a stand-in for a fact `gate_cap` (`gate.rs:59`) already proved. Thread the budget. **No-op proof** (the branch itself). | **−2** |
| **C2** | `provider/stream.rs:309` `1u64 << attempt.min(6)` | **Legitimate external bound, not debt**: the shift's own guard (`:307 attempt >= requests.retries`) does not cap the exponent, and `retries` is a `u32` **from disk** (`sections.rs:234`). `min(6)` protects against a config value. As-free rewrite available. | −1/+1 |
| **C6** | `agent_type.rs:106` `.filter(|v| !v.is_empty())` | **A real config defect**: `tools:` (empty) → `None` → `tools/surface.rs:45-51` gives the child the **full** tool base — a restriction key that silently unrestricts. Fix is semantics (`Some(vec![])` = empty surface), not deletion. No negative test exists. | 0 |
| **C7** | `agent_type.rs:115-122` `_ => ContextMode::Fresh` | `context_mode: compactedd` folds to `Fresh` silently — `skills` warns on the same class of violation (`skills/mod.rs:170-172`), `agent_type` does not. The fix (a warn) is net-positive → **hole**. | +4 |
| **C8** | `config/mod.rs:196-201` empty-`om.model` overlay | Root: the `""`-means-absent sentinel on `Om.model: String` (`sections.rs:341`), mirrored at `sessions.rs:23-26`. TOML's own absence idiom; adjudicated clean-adjacent. | +2 |
| **C9** | `DEFAULT_WINDOW` / `DEFAULT_MAX_OUTPUT` / `default_def()` trio | Three synthesizers of one pair (`sections.rs`, `agent/mod.rs:172-190`, `sessions.rs:553`), each justified at its site (the ACP channel declares no facts, `tau-acp/config.rs:70,203`). Name it once; **net 0** — the honest fix is naming, not deletion. | 0 |
| **C10** | `plan/mod.rs:137-143` two predicates, one legal-mark set | `mark.len() > 2` and `!done && !mark.is_empty() && mark != " "` spell the same set twice. Red: `plan/tests.rs:112` (insta). | **−3** |
| **C11** | `config/mod.rs:445-453` bare `[gates]` → `None` | The trim is load-bearing (`bash -c ""` exits 0 = an always-pass gate) — **but it lives at the merge site, not the type**: a code-built `Gates::default()` (`command: String::new()`) bypasses it. The invariant belongs in a `Gate` constructor. Net-positive → **hole**. | +2 |

## This pass's adjudicated-clean list is the audit's strongest

`provider/overflow.rs` (whole file, 246 LOC — unstructured HTTP bodies, byte scanning that cannot
slice-panic, saturating digit parsing, a number that only speaks when its prose names it; negatives at
`:234`/`:239`); `provider/stream.rs`'s `Ending` lattice (`:20-46`, three sources, one precedence, `#307`,
six tests); `provider/request.rs`'s `clamp_max_output`/`prompt_budget` (margin shared as one constant so
gate and clamp cannot drift, `:470-474`, pinned `wire.rs:150,164`); **all** of `config/`'s load-time
validation (`validate_providers`, `Om::validate`, `SubAgents::validate`, `spelling::validate`, the retired-key
ride-the-read checks, `LoadError::path()`) — **disk is the constructor for a config, so validation at load is
exactly the upstream place**; `plan`'s total parse function pinned by eight insta corpus snapshots;
`skills`' scope-ranked discovery with the byte-exact `SKILL.md` match; `gate.rs` (user-command output capped,
every failure folding to `passed: false` with a diagnostic); `context.rs`, `clock.rs`, `truncate.rs`.
**No parallel side tables in this scope** (brief item 5): the id-keyed companion-map cluster is confined to
`harness/` — the audit's root cause 1 does not infect `provider`/`config`/`skills`/`plan`.
**No producer/consumer atomic inversion in scope.**

## Pass 2 totals

**Net −281** (B3 −264, C1 −9, B1 −5, B2 −3, C3 −3, C4 −3, C10 −3, C5 −2; holes A1 +3, C7 +4, C8 +2,
C11 +2, C2/C6/C9 0). **A = 1, B = 3, C = 11.**
**Live defects: 1 confirmed** (A1's empty-model-key log spam — config-reachable, verified through every
hop) **+ 1 semantics defect** (C6's empty `tools:` unrestricting). **B3's release fall-through is refuted
as a defect** — documented, deliberate, and the finding is the doc contradiction, not the behaviour.
**6 no-op proofs** (A1's truncated-record test, C1 dead code, C2, C3, C5, C6, C7) → the audit's running
count is **21 no-op proofs across waves 4**, and pass 2 is the first pass where the *majority* of small
findings are untested by construction.

---

# FINAL VERDICT — all four waves closed (2026-11-04)

## The arithmetic, stated three ways because they are not the same number

**Production LOC removable: −1,358** (deliverable **−1,330 ± 30**).

| | Net LOC | Findings |
|---|---|---|
| Waves 1–3 (7 passes: `om`, `om_integration`, `harness` ×2, `agent`, `subagent`, `tau-acp`/`tau-protocol`) | **−785** | 51 |
| Frontend upgrades — FE1 −14→−21, FE4 −10→−30, FE6 −7→−12 (Addenda 4–5's deeper shifts supersede) | **+32** | 3 re-priced |
| Addendum 4 — `hashline` + `session` | **−138** | 9 priced, 1 refuted |
| Addendum 5 — `tools` + `tau-mock-llm` + frontend components | **−122** | 13 priced, 2 refuted |
| Addendum 6 — `provider` + `config` + `skills` + `plan` + top-level | **−281** | 15 (4 holes) |
| **Total** | **−1,358** | **88** |

**Of that −1,358, ~276 is relocation, not deletion.** B3 moves the scripted test providers out of the
release binary (production −276, test surface +276). **Whole-repo net (prod + test): ~−1,082.**
Tier A alone: **−583** across 25 findings — the structural set (1A −60, 1B −60, 1D −60, 1I −32, 2B −33,
2C −31, 2E −31, 3A −53, 3B −18, 4A −24, 4B −40, 4C −38, 4E −38, 4F −15, 5A −14, 5B −37, 5D −31, 5G −38,
5I −20, 6A −17, 6B −21, 6E −16, 6F −14, 6G −30, 6L −11) + Addenda' tier A (H1 −3, H2 −31, FE9 −21).
Addendum 6's tier-A item (A1) is **net-positive (+3)** and is in the report as a relocation, not a
reduction; its severity is earned by the defect, not by LOC.

**Coverage: 36,001 of 36,021 production LOC — ~100%.** Every crate in the workspace has now been read
line by line by a scoped pass. The finding count is no longer a floor.

**Live defects: 15.** 7 from waves 1–3 (headline: the streaming tool-latch defect, proven against the
shipped binary) + 3 from Addendum 4 (H1 path asymmetry, H2 dead CRLF output, H5's `expect` panic) +
3 from Addendum 5 (R3 background bash with no command exits 0 and is reported started, R4, F2's
stale highlight frames) + 2 from Addendum 6 (A1's empty-model-key permanent log spam, config-reachable and
verified through every hop; C6's empty `tools:` silently **un**restricting a child).

**21 no-op proofs.** Waves 4 ran 15 + 6: deleting the guard does not turn a test red. The pattern is
consistent — a patch is never tested; the tests that exist pin the patch (FE9b's trio is tested three
times against three implementations; Add4's T6 test *depends* on the hole it exposes; A1's
`a_truncated_model_record_answers_nothing` stays green with its guard deleted because the guard's stated
mechanism does not exist).

**14 claims refuted with receipts** (mine and the agents'): Add5's **R6 −44 → 0** (`report_detail` is live
and #243 shields the model from file bodies — the proposed shift is a privacy regression); Add5's R1
(`.max(1)` honors the schema's declared `"minimum": 1`); Add5's F1-interaction (a documented product rule);
Add4's **H11 −9 → 0** (no store retired-set query exists); Add6's **B3 severity B → C** and −268 → −276
(`canned.rs:16-73` is the live provider trait; and `forwarding.rs:222-223` documents the release
fall-through deliberately); 5F's enum; `in_flight_set` −12 → −3; 3B's A → B; my own 4A mechanism.

## The rule, unchanged after 36,021 LOC

**State must travel with the thing it describes.** Ten passes, six root causes, and every scope's deepest
finding is the same rule. It holds at every scale: `Core`'s 11 side tables (1A), `hashline::Registry`'s 7
(H5), `Mapper`'s 7 (1H), `ToolProgress`'s throttle fields (its constructors nearly fix it), `EntryCard`'s
`thinkSynced` (FE9), `Transcript`'s `openedObj` shadow map (FE9e), and the one config key `""` that
laundered through three hand-checks (A1). Pass 2 found **no** side-table cluster in
`provider`/`config`/`skills`/`plan` — the rule is not violated everywhere, which is why the fix set is
finite and the clean lists are long.

**Highest value per unit risk, unchanged:** (1) finish the #346 payload migration at the **readers**
(4A+4E+4C) — the type landed on the wire and the consumers never moved; every hand-decoder, `unwrap_or("")`,
and the latch defect grew in that gap. (2) The per-workspace aggregate (1A) — root of five findings and the
leak. (3) `#[cfg(test)]` on the scripted providers (B3) — a −276 single-line-boundary change that removes a
test seam from the release attack surface, the cheapest item in the audit.

**Three items need a human, not an agent:** T7 (three image config keys parsed, advertised, and never
honored — `encode_jpeg_q85` is hard-coded; honor them or delete them), 5G and 6C (spec decisions), and
A1/C6's config-surface semantics.

**Read-only, as briefed.** No repo file created, edited, or moved by any pass; every agent verified its
own tree clean; baseline green at `96d0d5c`. This file is **untracked** — committing it is your call.

---

# Measured Results — the three implementation lanes (2026-11-04)

The audit's proposals were implemented in three lanes and merged to main (`c510edb`). This section is the
ledger, measured, against the estimates above. **It refutes the report's central LOC claim**, and the pattern
of the refutation is the most useful thing in this document.

## The arithmetic, measured

Production LOC of the whole tree, every non-test file, inline `#[cfg(test)]` blocks brace-matched out
(`.tau/tree_loc.py <rev>`):

| Rev | Milestone | Production LOC |
|---|---|---|
| `c7ae1ac` | audit baseline | **45,157** |
| `2b0257f` | lane 1 (`tau-acp`/`tau-protocol`: 4A, 4E, 4C) | 45,334 |
| `9ac7c08` | lane 2 (cheap deletions + frontend, 15 items) | 45,280 |
| `fa1bd29` | lane 3 · 1A | 45,359 |
| `4b858ff` | lane 3 · 6K | 45,437 |
| `c510edb` | lane 3 · 1B | **45,554** |

**Campaign total: +397 production LOC.** The report's deliverable was **−923** (corrected from −905 at
line 1179). The gap is 1,320 lines, in the opposite direction.

Per-item, predicted → measured production net:

| Item | Predicted | Measured | Defect shape deleted | Live defect |
|---|---|---|---|---|
| 4A (typed payload readers) | −10 | inside +178 | 32 `unwrap_or("")` readers | — |
| 4E (tool phase from `status`) | −30 | **+77** | 3 latches + `finalized` set | **yes** (running→error) |
| 4C (typed subagent records) | −52 | **+120** | hand-parsed `ContextMode`, 6 defaults | **yes** (sibling provenance) |
| 1A (workspace aggregate) | −25 | **+79** | 8-map hand-sweep, `Drop` | **yes** (skills leak) |
| 6K (`Usage` derives its sum) | −8 | **+77** | 2 hand-syncs, `serde(default)` zero | **yes** (silent zero) |
| 1B (config state is one value) | −20 | **+156** | `""` sentinel key, 2-map agreement | — |
| Add6-C1 (dead field sweep) | −11 | **−11** | dead fields | — |
| Add6-C3 (dead `Vec<Ending>`) | −3 | **−3** | dead alloc | — |
| Add4-H2 (file split) | −31 | **−62** | (size) | — |
| FE10 ModelMenu | −12 | **−10** | overlay duplication | — |
| FE10 menu state | −8 | **+5** | — | — |
| Add6-B1/B2, Add5-T3/T4, 5E, Add4-H1/H7, FE9b | −90 | **+54** | mixed | **3 yes** |

Lanes: lane 1 **+173** (predicted −94), lane 2 **−69** (predicted −146), lane 3 **+312** (predicted −53).

## Why the estimates missed, and what it means

Two regimes, and the audit did not know there were two:

1. **Items that delete a file hit their price.** Add6-C1 −11/−11, Add6-C3 −3/−3, ModelMenu −10/−12, Add4-H2
   −62/−31. A dead field is a dead field; the estimate is a count.
2. **Items that touch a type grow the tree.** 4C +120, 4E +77, 1A +79, 1B +156, 6K +77. Every one of them
   deletes a guard and *adds the invariant that makes the guard unnecessary* — a newtype, an enum, a
   construction site, a `Drop`. The deletion is real; the lines it costs are not in the estimate.

So the report's rule — *"Every proposal is net-negative in production LOC"* — is false, and its failure is
informative: **an additive patch is a guard, and a change at the owner is a deletion, and the change at the
owner is the one that costs lines.** Net-deletion is a good *diagnostic* for spotting compensation logic
(it found all 88 findings) and a bad *objective* for pricing the fix. The bar that governs from here: **is
this genuinely an improvement to the codebase overall.**

## What the tranches bought (the return, not the cost)

**Eight live defects fixed** — each proven red before the fix, each a defect this audit predicted:

1. **4C** — `restore.rs` searched the head window first, so a fork child of any parent that had ever spawned
   a sub-agent read a **sibling's** provenance (`general`/`fresh` where the record said `auditor`/`fork`).
2. **4E** — a tool that finished was reported `running` for its whole life, so the ACP client saw a
   permanently in-flight call; `ToolPayload::status` is now the phase, `None` = in flight.
3. **1A** — the `skills` map keyed by workspace id was **never swept** by `close_workspace`: a permanent leak
   of every workspace's skill registry. Now one `remove`, with a leak test that is red without it.
4. **6K** — `{"input_tokens":3,"output_tokens":4}` decoded to `Ok(Usage{3,4,0,0})`: a partial usage became a
   **reported zero**, and the UI rendered a fabricated `0% cache` where the provider had said nothing. Now a
   decode error, and `StatusBar` renders `—`.
5. **Add4-H1** — `write` keyed the hashline release on the *raw* argument while `read`/`edit` key the
   resolved path: an ownership leak on any relative path.
6. **Add5-T3** — a background bash with no `command` spawned `bash -c ''` and **reported success**.
7. **FE9b** — stale highlight lines after a workspace switch.
8. **FE9e** — the open-tail one-shot was keyed on object identity, so any event patch (`touchChild`,
   `setArchived`, `applySubagentEvent`) that replaced the session record **re-paged a live session**. Fixed
   with an `opened` generation stamped by `openSession`, carried in `windowing.openedFor`.

**One new defect shipped and caught:** FE9e's *first* fix (keying re-open on object identity) was itself
wrong, discovered by writing the red test for the case the audit didn't name — event patches also replace
the record. This is the campaign's strongest methodological result: the fix was caught by the test written
for the adjacent case, not by the test for the reported one.

**Items the audit over-priced and are now recorded as refuted** (agents' own verdicts, not mine): 1B
initially proposed as a deletion (measured +156, kept on the improvement bar for the sentinel key and the
2-map agreement); FE9b-full at −60 (measured +27, shipped minimal at +8); FE9d's "count of arrivals" is a
fold, not a derivation; Add6-B3 is a **relocation**, not a deletion (needs `src-tauri` + `tau-test`
`Cargo.toml`), and stays a ticket.

**Gates on `c510edb`:** `cargo nextest run --workspace` 1176 tests (12 new); clippy `--workspace --all-targets -- -D warnings` exit 0; `cargo fmt --all -- --check` clean; vitest 55 files / 640 tests; eslint clean; `protocol-parity.mjs` OK (56 Rust surfaces). Note for CI: **bare `cargo clippy … -D warnings` is rejected by cargo 1.98** — pass it after `--`.

## The queue that remains

Judged by the improvement bar, not by line count: **T7** (three image config keys parsed, advertised,
never honored — `encode_jpeg_q85` hard-coded: honor or delete), **5G**/**6C** (spec decisions),
**Add6-B3** (feature-gate the scripted provider), **4D** (coordinated Rust+TS wire
change — typing `EntryPayload::Subagent` breaks every card's text without the TS side), **H3**/**H4** (need
a per-file anchor index: a feature), **H10** (`Loaded { at_len, entries }` across 7 sites), **FE1** (Svelte 6),
**FE4** (native menu — human decision), **T6** (`image_max_pixels: Option` → `u64` — a hole, net 0, needs
`TurnConfig` and the producer), the `Skill`/`SkillInfo` unification (6 files, net ~0). Done in wave 2:
**3B**, **1D**, **2B**, **2C**, **2E**, **Add6-A1**, **2D**, **FE2**, **FE7** (refuted).

# Wave 2 Measured Results (2026-11-05) — lanes A/B/C, merged

Three lanes in parallel worktrees on `267c288`, merged A → B → C (lane D, on lane C's files, follows).

| Rev | Milestone | Production LOC |
|---|---|---|
| `267c288` | wave 1 close (+ the parity-checker fix) | 45,570 |
| `ee2cedf` | lane B · 3B (`mint_id -> Result` + the append guard) | 45,599 |
| `a3cb606` | lane C · Add6-A1, 2D, FE2 | 45,642 |
| `ad55165` | lane B · 3B follow-up + 1D (the OM mirror is gone) | 45,678 |
| `240deec` | lane A · 2B + 2C + 2E (one stop flag, minted at the spawn) | **45,710** |

Wave 2 net **+140** production; campaign total **+553** against this report's **−923**.

| Item | Predicted | Measured | Defect shape deleted | Live defect |
|---|---|---|---|---|
| 2B + 2C (one child stop flag) | −12 | **+29** | the orphan flag: two flags per child, seam + agent | **yes** — a stopped child kept streaming |
| 2E (`self_weak`) | −8 | +3 | 7 silent-drop arms on a state the constructor forbids | — |
| 3B (`mint_id -> Result`) | −4 | **+29** | the swallow `let _ = self.ensure_open()` | **yes** — the 80 GB id-collision root |
| 3B follow-up | — | +12 | the mint's stale counter over a second store | **yes** — the append guard made it loud |
| 1D (OM buffer mirror) | **−60** | **+24** (code +17, doc +7) | the duplicate vec + the per-slice mirror sync | **yes** — the sync desynced index space |
| Add6-A1 (blank model key) | +3 | +32 | 3 hand-checks + a comment describing a deserializer that does not exist | **yes** — log spam every reopen |
| 2D (`skills_of`) | −3 | **−12** | the duplicate producer (two discovery walks) | **yes** (red 165 s deep) |
| FE2 (viewer staleness) | −2 | +23 | the field-compare guard — 3 sites, not the 2 reported | **yes** — the watcher's own re-read |

Method for this table: the whole-tree production LOC of each commit (`tree_loc.py`, which brace-matches
inline `#[cfg(test)]` blocks out), minus its parent. The agents' `git show --numstat` numbers differ where a
touched file carries a test module in the same file — 1D is the case: numstat said +92, the tree says +24,
because 71 of drain.rs's 110 added lines are a `#[cfg(test)]` module. The tree delta is the number; the
agent amended its own report to it. The `mod tests` hunk-header method agrees with the tree on every item
measured both ways.

**The two-regime rule, now eight items deep.** Every item that came in net-negative deletes a file-level
artifact (2D −12; wave 1's −11/−3). Every item that touches a *type* grows: 2B/2C +29, 3B +29, 1D +24,
Add6-A1 +32.
default, the latch, and the hand-sync sweep at the uses — the uses are the cheap part. The report's
estimates were systematically optimistic for exactly this reason: they counted the deleted guard and
never priced the type that makes it unnecessary.

**1D is the most over-priced item and the honest example.** Priced −60 to delete
`sync_buffer_mirror_from_durable`, a 6-line function. Deleting the mirror forces the durable read into the
drain slice, a 26-line read-through: **+24** measured (code +17, doc +7). Kept on the improvement bar: one
chunk store in one index space instead of two index-synced copies, and the desync they allowed was a live
defect with a red on each side of it.

**Gates on `240deec`.** `cargo nextest run -p tau-core --lib -p tau-protocol` **1090/1090**; `tau-acp`
`acp_protocol` + `acp_headless` **17/17 in 12.7 s**; `cargo fmt --all -- --check` clean; `cargo clippy
--workspace --all-targets -- -D warnings` exit 0; vitest 56 files / 645 tests; eslint clean;
`protocol-parity.mjs` OK (56 surfaces).

**Environment, recorded so a future red reads correctly.** `fseventsd` sits at 99% CPU after 11 days of
uptime on this box. Watcher tests run 126–230 s instead of seconds. `tau-acp` legs hang at exactly 120.0 s
under load, because nextest's default per-test timeout equals those tests' `READ_TIMEOUT` (120 s). A `HOME`
under `/var/folders` is unwatchable, so `session/new` blocks in notify's `watch_inner` → `stop()` →
`pthread_join` and answers nothing. The **pre-merge binary and the merged binary both answer 0/5** there,
which is what proves it is not a regression: `TMPDIR` under the home tree for every gate run; the cure is a
reboot (no sudo available here). One flake recorded: `session::tests_log::a_second_store_refreshes_before_parent_validation`
— green in ~96 isolated runs, red once in a full-workspace run at 0.008 s, in the two-stores-over-one-file
window that 3B's guard now refuses loudly.

# Wave 3 Measured Results (2026-11-06) — lanes D/E/F + merger items, merged

Lanes on `240deec`/`b9b8da6`, merged D → F → E. M1 and the 2C root half were done by the merger, not a lane.

| Rev | Milestone | Production LOC | Δ |
|---|---|---|---|
| `240deec` | wave 2 close | 45,710 | — |
| `f1c7f65` | merger · M3 (a mock capture that fails is reported) | 45,717 | +7 |
| `7995c85` | merger · the plan-file doc block owns its item | 45,714 | −3 |
| `dedc0a0` | lane D · T1 + T2 + T4 (the arg schema has a consumer) | 45,854 | **+140** |
| `c2a41bf` | merger · **2C root half** (one root stop flag) | 45,868 | +14 |
| `fe4bae9` | lane F · H10 (`Loaded` is the proof) | 46,010 | **+142** |
| `b9b8da6` | lane E · 2A (`SpawnNotice` carries the facts) | 46,028 | +18 |
| `cf5ed52` | merger · M1 (the mock's frames carry `created_at`) | 46,039 | +11 |

**Campaign production: 45,157 (wave 1 baseline) → 46,039 = +882.** This report promised −923. The sign is
the finding, not the arithmetic: see the two-regime rule in wave 2's notes, now confirmed on every item
of wave 3.

| Item | Predicted | Measured | Defect shape deleted | Live defect |
|---|---|---|---|---|
| T1 (arg schema) + T2 (verdict) | −25 | **+140** | 11 hand-parses → one module; the illegal `cancelled`∧`ok` pair has no construction site | **yes** — the background-branch double parse (T3) |
| H10 (`Loaded` proof) | −28 | **+142** | 7 hand `!loaded` guards → 3; the post-append stat | **yes** — found in lane: a duplicated `record_append` double-pushed every entry |
| 2A (`SpawnNotice`) | −39 | **+18** | 3 silent guards + the provider-smuggling registry | **yes** — #175: the fallback minted a second, non-agent provider per child |
| 2C root half | (−12, wave 2) | +14 | the paired write + its 4-line explanatory comment | **yes** — proven red on `240deec` by execution |
| M1 (mock `created_at`) | hole | +11 | the omitted-timestamp decode path standing in for the real one | fidelity, not a defect |
| M3 (mock capture) | −2 | +7 | the capture's swallow trio | yes — a missing witness file, reported far from the cause |

**Three lanes lost their work to the environment, and the reports did not say so.** Lane E's commit
`9c4f175` did not exist in the repository (branch tip `240deec`, 12 uncommitted files; the work survived
only as a repo stash and was merged as `73d5951`). Lane L's `c440b71` (4B, severity A) does not exist in
any object store, reflog, or dangling object — its 1201/1201 gate ran on a tree that is gone. A frontend
lane's FE9c (reported −135) was uncommitted when the OS cleared `/tmp`. **Every "green, committed" claim
in a lane report is now verified with `git cat-file -t` and `git status` before it reaches the ledger.**

**Two report-versus-tree disputes, settled by the tree.** Lane E's report said −138 for 2A; the tree
measured +18 (non-test code: bridge +38, canned +38, forwarding −53, launch −12, spawn +5). The FE lane
said −135 for FE9c; its tree measured net +5 with production −12. Both reports priced deleted lines and
left the added type out.

**Gate on `cf5ed52`:** nextest workspace **1197 passed / 0 failed**; fmt clean; clippy
`--workspace --all-targets -- -D warnings` exit 0.

## FE9c — refuted (2026-11-06, the third lane on this item)

The two verbatim `$effect(() => { if (ws) ensurePane(ws); })` in `LeftPane.svelte:19` and
`RightPane.svelte:22` are not a repair transition. Measured at source:

- `openWorkspaceInner` (`workspace.svelte.ts:118-125`) **already materializes the workspace** —
  `ensureWorkspace(store.files, real.id)` plus `void fetchDir(real.id, '.')`. A pane record and a root
  directory listing are two facts; neither subsumes the other. (A lane that swapped one for the other
  shipped an empty Files pane.)
- `ws` is `$derived(currentSession()?.meta.workspace ?? null)`; the pane read is `$derived(pane(ws))`.
  Both derive from the same state, so no render ever sees a non-null `ws` with an absent pane.
- The **only** producer that removes a pane is `closeWorkspace` (`workspace.svelte.ts:200`,
  `delete store.pane[ws.id]`), which also drops the workspace from `store.workspaces` in the same
  function — so `ws` goes null in the same tick. The effect fires for no producer, and `p` is
  null-guarded at every read site (11 sites in `SessionNode.svelte`, 5 in `LeftPane`, 3 in `RightPane`).
- Net measured on the implementing lane's tree: **+5** (production −12, test +17). The report's −138
  priced deleted lines and left the added `ensurePane` +17 out.

Verdict: the AGENTS.md `$effect` rule argument is real (the two effects *are* state sync, which the
rule bans), and deleting them is safe — but it is a **rule cleanup, not a debt shift**, it deletes no
null arm (they stay: `ws` is legitimately null with no session open), and it buys ~−12 production lines
that cost +17 test lines. Shipped as a rule fix if at all; not the −138 the report priced. This is the
same shape as 3E ("three repair transitions for a state no producer creates").

# Measured Results — waves 2–3, through `ff68cde` (2026-11-06)

Production LOC measured on the merge chain (`prod` = Rust outside inline `#[cfg(test)]` modules +
TS/Svelte; test files counted separately). Baseline `4b858ff` = **49,185**; head `ff68cde` = **50,060**
= **+875 production LOC**. Test files moved 6,778 → 6,982 (+204).

| Merge | Item | Production Δ | Audit priced |
|---|---|---|---|
| `ee2cedf` | 3B `mint_id -> Result`, 1D OM mirror | +231 | −62 |
| `a3cb606` | Add6-A1, 2D, FE2 | +43 | −18 |
| `ad55165` | lane B follow-up (mint refresh) | +121 | not priced |
| `240deec` | 2B, 2C-child, 2E | +32 | −41 |
| `f1c7f65` | M3 mock capture | +7 | −2 |
| `dedc0a0` | T1, T2, T4 (tool args, ToolStatus) | +202 | −70 |
| `c2a41bf` | 2C root half | +14 | −6 |
| `fe4bae9` | H10 log mirror | +142 | −28 |
| `b9b8da6` | 2A `SpawnNotice` typed | +18 | −138 |
| `56c9e0b` | 4B typed `OmStatusHook` | +10 | −4 |
| `0c7d217` | 2C total (every role rides one flag) | +9 | (regression cure) |
| `ff68cde` | 6A delta read propagates | +46 | −4 |

**Every item moved production LOC up; the audit projected −923.** The gap is structural, not
arithmetic. What a defect fix costs at this codebase's shape:

- **The swallow is the cheap path.** `.ok().flatten()` is 1 expression. Its replacement is a `match`, a
  named `System{Error}` event, a settle, and a return. Lane M's 6A: audit −4, measured +46 (turn.rs
  353→399) — because the fix *is* the error path.
- **A guard deleted is a type added.** 2A deletes 3 string guards and a provider-smuggling registry and
  adds a typed 11-field notice, its constructors, and its decode path: audit −138, measured +18.
- **Typed parsing costs more than `as_str()` hand-sweeps** (T1: +202 — one `tools/args.rs` replacing 11
  hand-parses, plus the spec-derived `required` arrays and the table test that pins them).
- **Red-capable tests are the price of every fix**, and they land in the test column (+204 files-worth).

The bar the campaign runs on is the one the user set: **genuine improvement, line count reported, never
the objective.** By that bar the wave is a win — 8 live defects closed (duplicate entry ids, the 80 GB
cycle, the orphaned child stop flag, the queue drains after a stop, silent empty deltas, the fork
provenance read, background-bash's missing-arg success, mock frames that hid `created_at`).

## 2C, completed: the queue red was a defect, not machine load

`harness::tests_queue::a_stop_with_queued_messages_leaves_them_and_starts_no_new_turn` was recorded in
this session's notes as load-induced ("a 120 s timeout under load is re-run with `--test-threads 1`
before calling red"). It is not. It is **9/10 red isolated in 6.7 s** on main.

Mechanism: pre-`c2a41bf`, `MessageStop` wrote *two* flags (`live.stop` and `live.agent.stop_flag()`)
under a comment naming the pair. Collapsing to one write was correct exactly where the flags were
shared — the `Root` and `Child` roles, which take a caller-minted flag. `Bare` and `Seam` minted their
agent's flag *inside* `launch`, unreachable to any caller, so the write reached only the stream cut:
the agent's durable stop never fell, and `process()`'s loop drained the queued messages a user stopped.

The fix makes flag identity **total** (`fa9c86b`, +9): every `SessionRole` carries a caller-minted
`stop`, the harness kit passes the flag its forwarding provider got, and `tests_launch` asserts
`Arc::ptr_eq` across the seam role so no role can mint its own again. 10/10 green on the fix, 6/6 on
merged main, full workspace gate **1201/1201, 0 fails** (acp legs included, machine quiet).

# Wave 4 Measured Results (2026-11-06) — lanes N/O/P + merger items, merged

Merge base `ff68cde` → main. Measured with `.tau/scripts/tree_loc.py` (whole-tree production,
inline `#[cfg(test)]` excluded).

| Merge | Item | prod net | Defect shape deleted | Live defect? |
|---|---|---|---|---|
| `1b1c4f9` | lane P · #326 store bag one source | **−24** | six hand-copied field lists | **yes** — 14 fields leaked between tests, verdict depended on file order |
| `70f1e08` | lane N · 3F the lane parameter | **−1** | `if steering` branch with no producer | no (coordination) |
| `9f5fc7e` | merger · FE9 card expansion | 0 | local clone + `$effect` + latch + dual-write | no (rule violation) |
| `bbd0550` | lane O · 5A/5B + 6D | **−16** | f64 round trip, dead `>= 1000.0` branch, two false `expect`s | **yes (6D)** — a promote left compressed tokens in the raw-window field the drain gate reads; a window that warranted a drain skipped it |

Wave-4 total: **−41 production**. Campaign through wave 4: 49,185 → **50,032** (+847).

## Refutations this wave (the report's own claims, killed by verification)

- **FE9's fix is impossible as written.** "Make `entryOpen` a reactive `$state(new Map())`" — Svelte
  5.57.2 does not deep-proxy a Map inside `$state`; a `.set` fires no reader. Pinned by
  `app/svelte/lib/store/state-proxy.test.ts` + `BagShapes.svelte`: a record key-add IS proxied, a Map
  `.set` is NOT. The store's "Maps are not reactive" comment block was accurate and stays. The fix
  changes the representation to the one the runtime tracks (a keyed record), not the Map.
- **3F's deletion variant is false.** The audit measured "forcing `entry_id: None` keeps all 406 green";
  it goes red at `harness::tests_stream` `a_notified_steer_lands_in_id_order…` (tests_stream.rs:457).
- **6E is not a mirror.** Two thresholds, not one: `buffered` is a hysteresis latch. Mutating it in
  place loses a frame — pinned by a new red-capable test (+7, refused as a deletion).
- **FE9e already dead** (the `openedObj` mirror went in `9ac7c08`); **FE9c refused a third time**
  (net +5 measured, needs a 16-file test migration; the prototype was reverted).

## Two process failures worth keeping

- **Lane reports naming commits that do not exist.** Two lanes reported shas absent from the repo
  (`3067452`, `80337a2`) with their work sitting uncommitted. On this box /tmp is wiped mid-flight —
  uncommitted work is lost work. Every lane brief now says: commit early, and `git cat-file -t <sha>`
  before reporting a sha.
- **File-tool anchors are content-hashed and shared across files.** An anchor taken from a grep of file
  X can be claimed by file Y and silently edit Y. Lane P lost 42 lines in `Transcript.svelte`; I lost a
  line in `tests_turns.rs`. Both recovered by `git diff` + `git checkout --`.
