use super::*;

use crate::agent::Lane;
use crate::session::SessionStore;
use crate::subagent::testkit::*;
use serde_json::json;
use std::time::Duration;

/// Drive the parent's loop: it runs its script, which calls the spawn
/// tools in-loop (the tool dispatch records the spawn entries in the
/// parent's session file).
async fn sup_spawn_from_parent(sup: &Arc<Supervisor>) {
    let parent = sup.parent.lock().as_ref().unwrap().upgrade().unwrap();
    parent.send("go", Lane::FollowUp);
    parent.process().await.unwrap();
}

/// A second supervisor over the same parent session — the parent's next
/// life, with no entries threaded through (the disk is its only evidence).
fn re_opened(
    dir: &std::path::Path,
    bridge: Arc<TestBridge>,
    factory: Arc<CannedFactory>,
    parent: &Arc<AgentSession>,
) -> Arc<Supervisor> {
    let sup = Supervisor::new(SupervisorParams {
        parent_session: "parent".into(),
        cwd: dir.to_path_buf(),
        provider: factory,
        model: "test-model".into(),
        system_prompt: "be terse".into(),
        om: Om::default(),
        tool_batch_on_force: ToolBatchPolicy::Complete,
        turn: TurnConfig::default(),
        gates: None,
        hashline: crate::hashline::pin_store::GcPolicy::default(),
        caps: SubAgents::default(),
        depth: 0,
        types: vec![crate::agent_type::builtin_general()],
        bridge,
        driver: Arc::new(TestDriver),
        parent_entries: None,
    });
    sup.attach_parent(parent);
    sup
}

/// A re-opened parent session must not re-mint a child handle an earlier
/// life already used (ticket #226): the GUI keys its roster rows by
/// handle, so a re-minted handle for a NEW child collides with a row the
/// frontend still carries for the old child, and the old child's card
/// ends up rendering the new sibling's state. The counter is seeded from
/// the `subagent_spawn` tool entries in the parent's own session file,
/// which is the durable spawn log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reopened_parent_does_not_remint_a_child_handle() {
    let dir = tempfile::tempdir().unwrap();
    // Life 1: the parent's loop spawns two children (one tool call each),
    // minting "parent-1" and "parent-2".
    let (sup, bridge, factory, parent) = harness_full(
        dir.path(),
        vec![
            sse(
                "",
                &[(
                    "subagent_spawn".into(),
                    "c1".into(),
                    spawn_args("first", "fresh").to_string(),
                )],
            ),
            sse(
                "",
                &[(
                    "subagent_spawn".into(),
                    "c2".into(),
                    spawn_args("second", "fresh").to_string(),
                )],
            ),
            sse("", &[]),
        ],
        vec![vec![sse("", &[])]],
        vec![Duration::ZERO],
        SubAgents::default(),
    );
    sup_spawn_from_parent(&sup).await;
    let mut handles = sup.handles();
    handles.sort();
    assert_eq!(handles, vec!["parent-1".to_owned(), "parent-2".to_owned()]);

    // Life 2: the session re-opens; a fresh supervisor adopts the same
    // parent session (the harness launch does this: the supervisor
    // adopts the parent's agent).
    let sup2 = re_opened(dir.path(), bridge, factory, &parent);
    let s3 = sup2
        .spawn("general", "third", Some(ContextMode::Fresh), "c3")
        .unwrap();
    // The new child's handle must not re-mint a handle an earlier life
    // already handed out (it would collide with a stale roster row).
    assert_ne!(
        s3.handle, "parent-1",
        "re-opened parent re-minted a previous life's handle"
    );
    assert_ne!(s3.handle, "parent-2");
    // Monotonic: a re-opened counter continues, it does not restart.
    assert_eq!(s3.handle, "parent-3");
}

/// The mint seed reads the handles the parent's children carry, not a tool
/// record's name string (#3H). The GUI's `subagent_spawn` command calls the
/// supervisor directly, so the parent's log names neither spawn — the durable
/// evidence is the minted handle on each child's own record, and a re-opened
/// supervisor mints past it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mint_seed_reads_the_handles_the_children_carry() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, bridge, factory, parent) = harness_full(
        dir.path(),
        vec![sse("x", &[])],
        vec![vec![sse("", &[])]],
        vec![Duration::ZERO],
        SubAgents::default(),
    );
    let first = sup
        .spawn("general", "a", Some(ContextMode::Fresh), "gui")
        .unwrap();
    let second = sup
        .spawn("general", "b", Some(ContextMode::Fresh), "gui")
        .unwrap();
    assert_eq!(first.handle, "parent-1");
    assert_eq!(second.handle, "parent-2");
    let mut pstore = SessionStore::for_workspace(dir.path(), "parent");
    pstore.open().unwrap();
    let tool_records = pstore
        .entries_range(0, usize::MAX)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EntryKind::Tool && e.payload["name"] == json!("subagent_spawn"))
        .count();
    assert_eq!(tool_records, 0, "the parent's log names neither spawn");

    // Life 2, no threaded entries: the children's records are the seed.
    let sup2 = re_opened(dir.path(), bridge, factory, &parent);
    let third = sup2
        .spawn("general", "c", Some(ContextMode::Fresh), "gui")
        .unwrap();
    assert_eq!(
        third.handle, "parent-3",
        "the seed followed the children's minted handles, not the empty tool log"
    );
}

/// The negative (#3H): a child whose record predates the handle field (#1K)
/// carries no counter to read, and a parent log with no tool record names
/// none — with no durable counter at all, the mint starts at 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_record_with_no_handle_contributes_nothing_to_the_seed() {
    let dir = tempfile::tempdir().unwrap();
    let (_sup, bridge, factory, parent) = harness_full(
        dir.path(),
        vec![sse("x", &[])],
        vec![vec![sse("", &[])]],
        vec![Duration::ZERO],
        SubAgents::default(),
    );
    let mut c = SessionStore::for_workspace(dir.path(), "old-child");
    c.create().unwrap();
    c.set_parent("parent").unwrap();
    c.append(
        EntryKind::Subagent,
        json!({
            "event": "spawn",
            "type": "general",
            "brief": "b",
            "context_mode": "fresh",
            "parent": "parent",
            "call": "c1"
        }),
        None,
    )
    .unwrap();

    let sup2 = re_opened(dir.path(), bridge, factory, &parent);
    let minted = sup2
        .spawn("general", "a", Some(ContextMode::Fresh), "gui")
        .unwrap();
    assert_eq!(
        minted.handle, "parent-1",
        "no durable counter, so the mint starts where a fresh parent starts"
    );
}

/// The spawn record carries the handle the parent minted (#1K), so a child's
/// address lives in the child's own file: the fleet's disk read names a
/// terminal child by the address the parent and the GUI use, not by its
/// session id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawn_record_carries_the_handle_the_parent_minted() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, _bridge, _parent) = harness(
        dir.path(),
        vec![sse("x", &[])],
        vec![vec![sse(
            "",
            &[notify_call("n1", "done", true, Some(json!({"r": 1})), None)],
        )]],
        SubAgents::default(),
    );
    let s = sup
        .spawn("general", "work", Some(ContextMode::Fresh), "c1")
        .unwrap();
    let info = wait_for_done(&sup, &s.handle);
    assert_eq!(info.handle, "parent-1", "the mint is the address");

    // The child is terminal, so the disk read lists it — with the minted
    // handle off its own record, and the record itself names it.
    let children = restore::disk_children(dir.path(), "parent");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].child, s.session_id);
    assert_eq!(
        children[0].handle, s.handle,
        "the disk child keeps the address the parent minted"
    );
    assert_eq!(
        children[0].spawn.as_ref().and_then(|r| r.handle.as_deref()),
        Some(s.handle.as_str()),
        "the spawn record is the address's durable home"
    );
}

/// The negative (#1K): every file on disk carries spawn records written
/// before the handle field, and those keep decoding. Such a child reads with
/// `handle: None` and keeps the address re-adopt has always produced for it —
/// its session id.
#[test]
fn a_spawn_record_with_no_handle_decodes_and_keeps_the_session_id_address() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = SessionStore::for_workspace(dir.path(), "p");
    p.create().unwrap();
    let mut c = SessionStore::for_workspace(dir.path(), "child-session");
    c.create().unwrap();
    c.set_parent("p").unwrap();
    // The pre-field record: no `handle` key, the shape every existing file holds.
    c.append(
        EntryKind::Subagent,
        json!({
            "event": "spawn",
            "type": "general",
            "brief": "b",
            "context_mode": "fresh",
            "parent": "p",
            "call": "c1"
        }),
        None,
    )
    .unwrap();
    c.append(
        EntryKind::Subagent,
        json!({ "event": "state", "state": "done" }),
        None,
    )
    .unwrap();

    let children = restore::disk_children(dir.path(), "p");
    assert_eq!(children.len(), 1);
    let record = children[0]
        .spawn
        .as_ref()
        .expect("a record with no handle still decodes");
    assert_eq!(record.agent_type, "general", "the provenance reads");
    assert_eq!(record.handle, None, "the field is absent, not invented");
    assert_eq!(
        children[0].handle, "child-session",
        "a child with no handle on record keeps its session id as address"
    );
}

/// The seed scans the parent's session file for spawn records; a fresh
/// parent (one spawn) counts from one, and the spawn is recorded as a
/// `subagent_spawn` tool entry — the seed's source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parent_without_spawn_records_starts_counting_from_one() {
    let dir = tempfile::tempdir().unwrap();
    let (sup, _, _parent) = harness(
        dir.path(),
        vec![
            sse(
                "",
                &[(
                    "subagent_spawn".into(),
                    "c1".into(),
                    spawn_args("only", "fresh").to_string(),
                )],
            ),
            sse("", &[]),
        ],
        vec![vec![sse("", &[])]],
        SubAgents::default(),
    );
    sup_spawn_from_parent(&sup).await;
    assert_eq!(sup.handles(), vec!["parent-1".to_owned()]);
    // The spawn is recorded as a `subagent_spawn` tool entry in the
    // parent's file (the durable spawn log the counter is seeded from).
    let mut store = SessionStore::for_workspace(dir.path(), "parent");
    store.open().unwrap();
    let spawns = store
        .entries_range(0, usize::MAX)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EntryKind::Tool && e.payload["name"] == json!("subagent_spawn"))
        .count();
    assert_eq!(spawns, 1, "the spawn record is the seed's source");
}
