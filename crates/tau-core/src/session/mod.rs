//! Session storage: one JSONL file per session (ADR-0005, spec §3) — a header
//! line plus an append-only `id`/`parentId` entry tree, per-line CRC,
//! zstd-compressed sidecar blobs for oversized payloads, and manual zstd
//! archives.
//!
//! The active leaf is the persisted branch choice (`set_leaf`) if any, else
//! the last entry line; the choice lives in the header, which is rewritten
//! atomically (entry lines are never touched — branching is always an append).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::log::Loaded;
pub(crate) use tau_protocol::EntryKind;

/// Session file format version — a versioned surface separate from the
/// protocol (spec §3, pi-style).
pub const FILE_VERSION: u32 = 1;

/// zstd level for blobs and archives — the level measured in ADR-0005.
const ZSTD_LEVEL: i32 = 3;

/// The sidecar-blob threshold (spec §3: 100 KB — the research
pub const DEFAULT_BLOB_THRESHOLD: u64 = 100_000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    id: String,
    created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leaf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// The creator session (sub-agent provenance, ADR-0001); the GUI nests
    /// the session under its parent in the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

/// A session file: header + append-only entry lines (ADR-0005).
///
/// One writer per session; reads are bounded (`entries_range`, `entries_since`,
/// `entries_tail`) — there is deliberately no full-dump read API (spec §3
/// protocol rule).
pub(crate) type EntryEventHook = Arc<dyn Fn(&Entry) + Send + Sync>;

pub struct SessionStore {
    id: String,
    root: PathBuf,
    fixed_time: Option<u64>,
    leaf: Option<String>,
    created: u64,
    title: Option<String>,
    parent: Option<String>,
    /// The in-memory log and the file length it is proven at (`Loaded`,
    /// H10): `None` until this store has read the file, so every read knows
    /// whether it has a mirror to serve from, and a mirror read carries the
    /// length it is valid at.
    log: Option<Loaded>,
    /// The load generation: `open` bumps it, so a caller's cache of loaded-log
    /// rows can tell one file from the next under the same session id
    /// (`Skeleton`'s identity, #316).
    load_seq: u64,
    /// A live entry sink (the app maps each appended entry to a protocol
    /// event). Set by the owning session; `None` for stores with no live
    /// surface. Fired at the append choke point so every write path — the
    /// turn loop and the OM — emits.
    entry_event_hook: Option<EntryEventHook>,
}

/// Storage errors.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    /// A line failed its CRC check; `line` is the 1-based file line number
    /// (the header is line 1).
    Corrupt {
        line: usize,
        expected: String,
        actual: String,
    },
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Json(e) => write!(f, "json: {e}"),
            Self::Corrupt {
                line,
                expected,
                actual,
            } => write!(
                f,
                "corrupt session line {line}: expected crc {expected}, found {actual}"
            ),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl SessionStore {
    /// Workspace-scoped store: `{project root}/.tau/` (spec §3 placement).
    #[must_use]
    pub fn for_workspace(project_root: &Path, id: &str) -> Self {
        Self::new(project_root.join(".tau"), id)
    }

    fn new(root: PathBuf, id: &str) -> Self {
        Self {
            id: id.to_owned(),
            root,
            fixed_time: None,
            leaf: None,
            created: 0,
            title: None,
            parent: None,
            log: None,
            load_seq: 0,
            entry_event_hook: None,
        }
    }

    /// Set the live entry sink (the app maps each appended entry to a
    /// protocol event); `None` clears it.
    pub fn set_entry_event_hook(&mut self, hook: Option<EntryEventHook>) {
        self.entry_event_hook = hook;
    }
    /// A fixed timestamp for every write (the test seam that makes the
    /// shared fixture byte-deterministic, roadmap G handoff 1).
    #[must_use]
    pub const fn with_fixed_time(mut self, ms: u64) -> Self {
        self.fixed_time = Some(ms);
        self
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The header's created timestamp (epoch ms).
    #[must_use]
    pub const fn created(&self) -> u64 {
        self.created
    }

    /// The header's title (a readable session name; absent on old files).
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("{}.jsonl", self.id))
    }

    fn now_ms() -> u64 {
        crate::clock::now_ms()
    }
    pub(crate) fn now(&self) -> u64 {
        self.fixed_time.unwrap_or_else(Self::now_ms)
    }

    /// The next entry id, reserved now (the single id counter, ADR-0008):
    /// a wire-only re-emission can be minted before its file line lands, and
    /// the later append reuses the same id.
    ///
    /// The open is the invariant, not a step to try: an id only means
    /// "past this file's ids" once the file is read, so a failed open is a
    /// failed mint. Swallowing it issued `00000001` — every session file's
    /// first id — from an unread store, and the duplicate id is what the
    /// parent-chain walk loops on (the live-om 80 GB runaway).
    ///
    /// The read is a growth refresh, not merely "open once": a handle kept
    /// alive over a file a second store has appended (the harness re-opens
    /// live sessions; fork/branch rewrite through their own stores) has a
    /// counter behind the file, and the id it reserves is the one the other
    /// handle writes next.
    pub fn mint_id(&mut self) -> Result<String, Error> {
        self.refresh_if_grown()?;
        // The counter lives in the mirror, so a mint has read the file by
        // construction: `refresh_if_grown` opens when there is no mirror, and
        // a failed open returned above (H10).
        let log = self
            .log
            .as_mut()
            .expect("refresh_if_grown leaves a loaded log");
        Ok(format!("{:08}", log.take_next()))
    }

    fn new_entry(
        &self,
        id: &str,
        kind: EntryKind,
        payload: Value,
        first_kept_entry_id: Option<String>,
    ) -> Entry {
        Entry {
            id: id.to_owned(),
            parent: None,
            timestamp: self.now(),
            kind,
            payload,
            blob: None,
            first_kept_entry_id,
            crc: None,
        }
    }

    /// Create a new session file with its header line. Fails if it exists.
    pub fn create(&mut self) -> Result<(), Error> {
        if self.path().exists() {
            return Err(Error::Other(format!("session {} already exists", self.id)));
        }
        fs::create_dir_all(self.root.join("sessions"))?;
        let created = self.now();
        let header = Header {
            kind: "session".into(),
            version: FILE_VERSION,
            id: self.id.clone(),
            created,
            leaf: None,
            title: None,
            parent: None,
        };
        self.created = created;
        let mut line = serde_json::to_string(&header)?;
        line.push('\n');
        fs::write(self.path(), &line)?;
        // This store wrote the file: it holds the mirror of what the file
        // holds (nothing) at the length it wrote (H10).
        self.log = Some(Loaded::new(
            u64::try_from(line.len()).expect("file length, well under u64::MAX"),
        ));
        Ok(())
    }

    /// Load the header and every entry, verifying each line's CRC, into the
    /// store's mirror `Loaded` — the entries plus the file length they are
    /// proven at (H10).
    ///
    /// A torn last line (process killed mid-append) is skipped in memory —
    /// the read path never rewrites the file (the writer settles the tail
    /// at the next append); any other bad line is corruption. A failed open
    /// leaves the store with no mirror, so the failed read that follows it
    /// reaches no rows.
    pub fn open(&mut self) -> Result<(), Error> {
        self.load_seq += 1;
        // A store mid-open has no mirror: the load generation moved, and the
        // rows the old one held describe the file this open has not read.
        self.log = None;
        let raw = fs::read_to_string(self.path()).map_err(|e| Error::Other(e.to_string()))?;
        let mut log =
            Loaded::new(u64::try_from(raw.len()).expect("file length, well under u64::MAX"));
        let lines = Self::entry_lines(&raw);
        let Some(header_line) = lines.first() else {
            return Err(Error::Other("session file has no header".into()));
        };
        let header: Header = serde_json::from_str(header_line)?;
        if header.kind != "session" {
            return Err(Error::Other("first line is not a session header".into()));
        }
        if header.version != FILE_VERSION {
            return Err(Error::Other(format!(
                "unsupported session file version {}",
                header.version
            )));
        }
        if header.id != self.id {
            return Err(Error::Other(format!(
                "header id {} does not match file id {}",
                header.id, self.id
            )));
        }
        self.leaf.clone_from(&header.leaf);
        self.created = header.created;
        self.title = header.title;
        self.parent = header.parent;

        for (i, line) in lines.iter().enumerate().skip(1) {
            if line.is_empty() {
                log.push_empty();
                continue;
            }
            let entry: Entry = match line.parse::<Entry>() {
                Ok(e) => e,
                Err(_) => return Err(Error::Other(format!("malformed entry on line {}", i + 1))),
            };
            entry.verify(i + 1)?;
            log.push_entry(entry, line.len());
        }
        self.log = Some(log);
        Ok(())
    }

    /// The mirror opens on demand: a store that has never read its file reads
    /// it now, so a write path learns the file's id set before it writes.
    fn ensure_open(&mut self) -> Result<(), Error> {
        if self.log.is_none() {
            self.open()?;
        }
        Ok(())
    }

    /// Paged read by 0-based entry index (the header is not an entry).
    pub fn entries_range(&self, start: usize, end: usize) -> Result<Vec<Entry>, Error> {
        let raw = fs::read_to_string(self.path()).map_err(|e| Error::Other(e.to_string()))?;
        let mut out = Vec::new();
        for (i, line) in Self::entry_lines(&raw).iter().skip(1).enumerate() {
            if line.is_empty() {
                continue;
            }
            if i >= end {
                break;
            }
            if i >= start {
                let entry: Entry = line.parse::<Entry>()?;
                entry.verify(i + 2)?;
                out.push(entry);
            }
        }
        Ok(out)
    }

    /// The last `n` entries in append order (oldest first), taken from the
    /// file's end: the lines are walked backwards so only the tail window is
    /// parsed and CRC-verified (spec §3: tail reads stay cheap). `n` over the
    /// log's length is the whole log; `n == 0` is empty. This is file order,
    /// not the active branch — a forked session's abandoned branch can sit at
    /// the file's end, so a branch caller walks from the leaf.
    pub fn entries_tail(&self, n: usize) -> Result<Vec<Entry>, Error> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(self.path()).map_err(|e| Error::Other(e.to_string()))?;
        let lines = Self::entry_lines(&raw);
        let mut picked: Vec<(usize, &str)> = Vec::new();
        for (i, line) in lines.iter().skip(1).enumerate().rev() {
            if picked.len() == n {
                break;
            }
            if line.is_empty() {
                continue;
            }
            picked.push((i, &**line));
        }
        picked.reverse();
        let mut out = Vec::with_capacity(picked.len());
        for (i, line) in picked {
            let entry: Entry = line.parse::<Entry>()?;
            entry.verify(i + 2)?;
            out.push(entry);
        }
        Ok(out)
    }

    /// Paged read of the entries after `cursor` (exclusive). The cursor line
    /// is located by `"id"` prefix scan (entry ids serialize first, in
    /// append order) so tail reads never parse the entries before the
    /// cursor (spec §3: tail reads stay cheap); entries after it are parsed
    /// and CRC-verified as usual.
    pub fn entries_since(&self, cursor: &str) -> Result<Vec<Entry>, Error> {
        let raw = fs::read_to_string(self.path()).map_err(|e| Error::Other(e.to_string()))?;
        let mut out = Vec::new();
        let prefix = format!("{{\"id\":\"{cursor}\"");
        let mut past = false;
        for (i, line) in Self::entry_lines(&raw).iter().skip(1).enumerate() {
            if line.is_empty() {
                continue;
            }
            if past {
                let entry: Entry = line.parse::<Entry>()?;
                entry.verify(i + 2)?;
                out.push(entry);
            } else if line.starts_with(&prefix) {
                past = true;
            }
        }
        Ok(out)
    }

    /// Read one entry by id.
    pub fn entry(&self, id: &str) -> Result<Entry, Error> {
        let raw = fs::read_to_string(self.path()).map_err(|e| Error::Other(e.to_string()))?;
        for (i, line) in Self::entry_lines(&raw).iter().skip(1).enumerate() {
            if line.is_empty() {
                continue;
            }
            let entry: Entry = line.parse::<Entry>()?;
            if entry.id == id {
                entry.verify(i + 2)?;
                return Ok(entry);
            }
        }
        Err(Error::Other(format!("no entry {id}")))
    }
}

mod branch;
mod entry;
mod file;
mod list;
mod log;

pub use branch::branch;
pub use entry::{BlobRef, Entry};
pub use list::list_workspace;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_branch;
#[cfg(test)]
mod tests_log;
