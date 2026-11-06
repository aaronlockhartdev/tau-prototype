use super::*;

use serde_json::json;

/// Three entries written to disk by one store; the returned store is a
/// fresh handle on the same file that has never been `open`ed
/// (`log == None` — it has no mirror of its own).
fn unopened_store_on_a_file(dir: &Path) -> (SessionStore, Vec<String>) {
    let mut writer = SessionStore::for_workspace(dir, "s1");
    writer.create().unwrap();
    let mut ids = Vec::new();
    let mut parent: Option<String> = None;
    for t in ["one", "two", "three"] {
        let e = writer
            .append(
                EntryKind::User,
                json!({ "text": t, "lane": "follow-up" }),
                parent.as_deref(),
            )
            .unwrap();
        parent = Some(e.id.clone());
        ids.push(e.id);
    }
    (SessionStore::for_workspace(dir, "s1"), ids)
}

#[test]
fn cached_reads_fall_back_to_the_file_when_the_store_is_unopened() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, ids) = unopened_store_on_a_file(dir.path());

    // entries_range_cached: the whole file, then a paged slice.
    let all = store.entries_range_cached(0, usize::MAX).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].0.id, ids[0]);
    let page = store.entries_range_cached(1, 2).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].0.id, ids[1]);

    // entries_since_cached: everything after the cursor.
    let since = store.entries_since_cached(&ids[0]).unwrap();
    assert_eq!(since.len(), 2);
    assert_eq!(since[0].id, ids[1]);

    // entry_cached: a hit and a miss.
    assert_eq!(store.entry_cached(&ids[2]).unwrap().id, ids[2]);
    assert!(store.entry_cached("no-such-entry").is_err());

    // leaf_cached: the last entry in the file.
    let leaf = store.leaf_cached().unwrap().unwrap();
    assert_eq!(leaf.id, ids[2]);
}

#[test]
fn refresh_if_grown_reopens_when_the_file_grew() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _ids) = unopened_store_on_a_file(dir.path());
    // Unopened: the refresh is a full open (the no-mirror arm).
    store.refresh_if_grown().unwrap();
    // Now loaded and current: a no-op stat.
    store.refresh_if_grown().unwrap();
    assert_eq!(store.entries_range_cached(0, usize::MAX).unwrap().len(), 3);
}

#[test]
fn a_second_store_refreshes_before_parent_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = SessionStore::for_workspace(dir.path(), "s1");
    writer.create().unwrap();
    let e1 = writer
        .append(EntryKind::User, json!({ "text": "one" }), None)
        .unwrap();
    // A second store opens while the file holds one entry: its in-memory
    // id set is current only until the writer appends again.
    let mut other = SessionStore::for_workspace(dir.path(), "s1");
    other.open().unwrap();
    let e2 = writer
        .append(EntryKind::User, json!({ "text": "two" }), Some(&e1.id))
        .unwrap();
    // The stale store must refresh against the grown file before the
    // parent check, not refuse a valid parent as "unknown".
    let e3 = other
        .append(EntryKind::User, json!({ "text": "three" }), Some(&e2.id))
        .unwrap();
    assert_eq!(e3.parent.as_deref(), Some(e2.id.as_str()));
    // A genuinely unknown parent is still refused.
    let err = other
        .append(
            EntryKind::User,
            json!({ "text": "four" }),
            Some("no-such-entry"),
        )
        .unwrap_err();
    assert!(err.to_string().contains("unknown parent"), "{err}");
}

/// H10's contract for `Loaded`: the mirror names the file length it is valid
/// at, so the freshness check is the only re-open and a cached read never
/// serves rows the file has outgrown. This is the property the `loaded` flag
/// guarded by hand at seven read sites; the length now travels with the rows.
#[test]
fn a_cached_read_reflects_a_file_a_second_store_grew() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = SessionStore::for_workspace(dir.path(), "s1");
    writer.create().unwrap();
    let first = writer
        .append(EntryKind::User, json!({ "text": "one" }), None)
        .unwrap();
    let mut reader = SessionStore::for_workspace(dir.path(), "s1");
    reader.open().unwrap();

    // Positive: a store proven at the file's length answers from its mirror
    // and pays no re-open.
    let opened = reader.load_seq();
    reader.refresh_if_grown().unwrap();
    assert_eq!(
        reader.load_seq(),
        opened,
        "a store proven at the file's length re-opened: its proof is behind the file"
    );

    // Negative: the file grows past the proof — the cached reads take the
    // grown file, not the mirror that predates it.
    let second = writer
        .append(EntryKind::User, json!({ "text": "two" }), Some(&first.id))
        .unwrap();
    reader.refresh_if_grown().unwrap();
    assert!(
        reader.load_seq() > opened,
        "a file that grew past the proof opened no new load"
    );
    assert_eq!(reader.entry_count(), 2);
    assert_eq!(reader.leaf_cached().unwrap().unwrap().id, second.id);
    assert_eq!(
        reader
            .entries_tail_cached(1)
            .unwrap()
            .iter()
            .map(|e| e.id.clone())
            .collect::<Vec<_>>(),
        vec![second.id]
    );
}

/// The append is the writer, so the bytes it wrote are the proof it keeps:
/// `file.rs` used to `stat` the file after every append to re-sync a
/// hand-kept length (H10 deletes that). The arithmetic is load-bearing — a
/// proof left behind the file re-opens on every read, and one left ahead of it
/// serves a stale log. A torn tail is the case that moves the file by
/// something other than the appended line.
#[test]
fn an_append_proves_the_length_it_writes_including_a_settled_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = SessionStore::for_workspace(dir.path(), "s1");
    store.create().unwrap();
    store
        .append(EntryKind::User, json!({ "text": "one" }), None)
        .unwrap();
    let opened = store.load_seq();
    store.refresh_if_grown().unwrap();
    assert_eq!(
        store.load_seq(),
        opened,
        "the append left the mirror's proof behind the file: the read re-opened"
    );

    // A kill mid-append leaves a torn last line; the next append truncates it
    // before writing, so the file shrinks by the torn bytes and grows by the
    // line. The proof has to move by both.
    let mut raw = fs::read_to_string(store.path()).unwrap();
    raw.push_str("{\"id\":\"00000009\",\"tam");
    fs::write(store.path(), raw).unwrap();
    store
        .append(EntryKind::User, json!({ "text": "two" }), None)
        .unwrap();
    let opened = store.load_seq();
    store.refresh_if_grown().unwrap();
    assert_eq!(
        store.load_seq(),
        opened,
        "a settled tail left the mirror's proof off the file's length"
    );
    assert_eq!(store.entry_count(), 2, "the settled tail kept a slot");
}

/// The mirror is the file's shape, slot for slot: an append takes one slot,
/// never two. H10 moves the mirror's rows into the append path, and a paging
/// index that stops matching the file's lines strands every paged read on the
/// wrong window (the snapshot's range reads, the GUI's viewport).
#[test]
fn an_append_takes_one_mirror_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = SessionStore::for_workspace(dir.path(), "s1");
    store.create().unwrap();
    let mut parent: Option<String> = None;
    for i in 0..5u32 {
        let e = store
            .append(
                EntryKind::User,
                json!({ "text": format!("msg {i}") }),
                parent.as_deref(),
            )
            .unwrap();
        parent = Some(e.id);
    }
    // The file holds six lines, the header is not a slot, so the mirror holds
    // five — and its ids are the file's entry lines, each once, in order.
    assert_eq!(store.entry_count(), 5);
    let ids: Vec<String> = store
        .entries_range_cached(0, usize::MAX)
        .unwrap()
        .into_iter()
        .map(|(e, _)| e.id)
        .collect();
    assert_eq!(
        ids,
        (1..=5u32).map(|i| format!("{i:08}")).collect::<Vec<_>>(),
        "the mirror's slots are not the file's entry lines"
    );
    // A window read answers with the entries it names, not their neighbours.
    let window = store.entries_range_cached(1, 3).unwrap();
    assert_eq!(
        window
            .iter()
            .map(|(e, _)| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["00000002", "00000003"]
    );
}

/// A store that has never read its file has no mirror, so it has no rows of
/// its own: the counts that index the mirror are empty and every read reaches
/// the file. H10's negative side — a cached read on an unproven store cannot
/// be served stale, because there is no mirror to serve it from.
#[test]
fn an_unopened_store_has_no_rows_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (store, ids) = unopened_store_on_a_file(dir.path());

    assert_eq!(store.entry_count(), 0, "a cold store reported rows");
    assert_eq!(store.load_seq(), 0, "a cold store reported a load");
    // The reads are the file's, and they carry no mirror line sizes (0 is the
    // file-backed read's answer for a size it did not read).
    let mut seen = Vec::new();
    store
        .visit_entries_cached(|e, size| seen.push((e.id.clone(), size)))
        .unwrap();
    assert_eq!(
        seen.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(
        seen.iter().all(|(_, size)| *size == 0),
        "the cold store served mirror sizes: {seen:?}"
    );
}
