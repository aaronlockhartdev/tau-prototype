//! The core tools' arguments, parsed once (audit Add5-T1).
//!
//! The schema in `mod.rs` used to declare each tool's `required` set as prose
//! for the model while the enforced contract was a hand-parse at every entry
//! point, and nothing read `.schema` to keep the two honest. The required keys
//! are named once, here: `require` refuses a call that lacks one, and
//! `mod.rs` builds each spec's `required` array from the same list. One
//! contract, one place to change it. Every message a model can be sent for a
//! bad core-tool call lives in this module.

use crate::hashline;
use serde_json::Value;

/// `read`: the schema's required set. The bound type is a string; `offset` and
/// `limit` are the optional page window.
pub(super) const READ_REQUIRED: [&str; 1] = ["path"];
/// `write`: the schema's required set.
pub(super) const WRITE_REQUIRED: [&str; 2] = ["path", "content"];
/// `edit`: the schema's required set (`edits` carries `minItems: 1`). The two
/// keys are named once, so the parser and the spec cannot spell them apart.
pub(super) const EDIT_REQUIRED: [&str; 2] = [EDIT_PATH, EDIT_EDITS];
const EDIT_PATH: &str = "path";
const EDIT_EDITS: &str = "edits";
/// One `edit` member: the schema's required set for an `edits` item.
pub(super) const EDIT_MEMBER_REQUIRED: [&str; 3] = ["from", "to", "content"];
/// `bash`: the schema's required set.
pub(super) const BASH_REQUIRED: [&str; 1] = ["command"];

/// The one enforcement of a tool's declared `required` set, in list order so
/// the first missing key is the one named back to the model.
fn require(tool: &str, args: &Value, keys: &[&str]) -> Result<(), String> {
    keys.iter()
        .find(|key| args.get(*key).is_none())
        .map_or_else(|| Ok(()), |key| Err(format!("{tool}: missing {key:?}")))
}

fn str_field<'a>(tool: &str, args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{tool}: missing {key:?}"))
}

fn bool_field(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// `read`: the required `path` plus the optional page window.
pub(super) struct ReadArgs {
    pub path: String,
    pub offset: Option<String>,
    pub limit: Option<usize>,
}

impl ReadArgs {
    pub(super) fn parse(args: &Value) -> Result<Self, String> {
        require("read", args, &READ_REQUIRED)?;
        Ok(Self {
            path: str_field("read", args, READ_REQUIRED[0])?.to_owned(),
            offset: args
                .get("offset")
                .and_then(Value::as_str)
                .map(str::to_owned),
            limit: args
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok()),
        })
    }
}

/// `write`: the required `path` and `content`.
pub(super) struct WriteArgs {
    pub path: String,
    pub content: String,
}

impl WriteArgs {
    pub(super) fn parse(args: &Value) -> Result<Self, String> {
        require("write", args, &WRITE_REQUIRED)?;
        Ok(Self {
            path: str_field("write", args, WRITE_REQUIRED[0])?.to_owned(),
            content: str_field("write", args, WRITE_REQUIRED[1])?.to_owned(),
        })
    }
}

/// `edit`: the required `path` and `edits` (the schema's `minItems: 1`), and
/// the declared `strict` default.
pub(super) struct EditArgs {
    pub path: String,
    pub edits: Vec<hashline::Edit>,
    pub strict: bool,
}

impl EditArgs {
    pub(super) fn parse(args: &Value) -> Result<Self, String> {
        require("edit", args, &EDIT_REQUIRED)?;
        let path = str_field("edit", args, EDIT_REQUIRED[0])?.to_owned();
        let Some(members) = args.get(EDIT_EDITS).and_then(Value::as_array) else {
            return Err(format!("edit: {EDIT_EDITS:?} array is required"));
        };
        if members.is_empty() {
            return Err(format!(
                "edit: {EDIT_EDITS:?} must contain at least one edit"
            ));
        }
        let mut edits = Vec::with_capacity(members.len());
        for (i, item) in members.iter().enumerate() {
            let [Ok(from), Ok(to), Ok(content)] =
                EDIT_MEMBER_REQUIRED.map(|key| str_field("edit", item, key))
            else {
                return Err(format!(
                    "edit: edits[{i}] requires \"from\", \"to\", and \"content\""
                ));
            };
            edits.push(hashline::Edit {
                from: from.to_owned(),
                to: to.to_owned(),
                content: content.to_owned(),
            });
        }
        Ok(Self {
            path,
            edits,
            strict: bool_field(args, "strict", false),
        })
    }
}

/// `bash`: the required `command`, its `timeout_secs` default and the
/// declared `minimum: 1` (the clamp honors that bound, adjudicated in the
/// audit), and the declared `background` default.
pub(super) struct BashArgs {
    pub command: String,
    pub timeout_secs: u64,
    pub background: bool,
}

impl BashArgs {
    pub(super) fn parse(args: &Value) -> Result<Self, String> {
        require("bash", args, &BASH_REQUIRED)?;
        Ok(Self {
            command: str_field("bash", args, BASH_REQUIRED[0])?.to_owned(),
            timeout_secs: args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(60)
                .max(1),
            background: bool_field(args, "background", false),
        })
    }
}

/// A core tool's arguments, parsed from the raw call. `None` is a name that is
/// not a core tool; `Err` is a contract violation of one that is.
pub(super) enum CoreArgs {
    Read(ReadArgs),
    Write(WriteArgs),
    Edit(EditArgs),
    Bash(BashArgs),
}

pub(super) fn parse_core(name: &str, args: &Value) -> Result<Option<CoreArgs>, String> {
    match name {
        "read" => ReadArgs::parse(args).map(|a| Some(CoreArgs::Read(a))),
        "write" => WriteArgs::parse(args).map(|a| Some(CoreArgs::Write(a))),
        "edit" => EditArgs::parse(args).map(|a| Some(CoreArgs::Edit(a))),
        "bash" => BashArgs::parse(args).map(|a| Some(CoreArgs::Bash(a))),
        _ => Ok(None),
    }
}
