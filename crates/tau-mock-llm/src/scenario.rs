use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One scripted turn: a streamed text, a batch of tool calls, or both.
#[derive(Debug, Clone, Deserialize)]
pub struct Turn {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub calls: Vec<Call>,
}

/// One tool call; `arguments` is a full JSON object in the file and is
/// serialized to the complete-JSON-string wire shape at render time.
#[derive(Debug, Clone, Deserialize)]
pub struct Call {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl Default for Usage {
    fn default() -> Self {
        Usage {
            input_tokens: 120,
            output_tokens: 48,
            total_tokens: 168,
        }
    }
}

/// A scenario: a content match plus a sequential turn list.
#[derive(Debug, Deserialize)]
pub struct Scenario {
    #[serde(rename = "match")]
    pub pattern: String,
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(skip)]
    counter: AtomicUsize,
}

impl Scenario {
    pub fn new(pattern: impl Into<String>, turns: Vec<Turn>, usage: Usage) -> Self {
        Self {
            pattern: pattern.into(),
            turns,
            usage,
            counter: AtomicUsize::new(0),
        }
    }

    /// Next turn, sequential per scenario; a request that lands past the
    /// end of the script clamps to the final turn (a stuck client repeats
    /// the ending instead of hanging).
    pub fn next_turn(&self) -> (usize, &Turn) {
        // load_dir rejects an empty script; the guard is for direct `new`
        // callers (an empty `turns` would underflow the clamp).
        assert!(!self.turns.is_empty(), "a scenario needs at least one turn");
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let i = n.min(self.turns.len() - 1);
        (i, &self.turns[i])
    }

    /// Replay the script from the top (a wdio spec retry must not hit the
    /// clamped final turn).
    pub fn reset(&self) {
        self.counter.store(0, Ordering::SeqCst);
    }
}

/// The request fields selection reads: the system instructions and the
/// input items (messages carry `role`/`content`; the call/output item
/// shapes are irrelevant to matching).
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct WireRequest {
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub input: Vec<Value>,
}

/// Built-in answer for a request no scenario claims: short, terminated,
/// deterministic — a spec can never hang the stream.
#[must_use]
pub fn fallback() -> Scenario {
    Scenario {
        pattern: String::new(),
        turns: vec![Turn {
            text: Some(
                "mock fallback: no scenario matched this request; the turn ends here.".into(),
            ),
            calls: vec![],
        }],
        usage: Usage::default(),
        counter: AtomicUsize::new(0),
    }
}

/// All scenarios of one `--scenarios` directory, in file-name order (the
/// deterministic match priority).
pub struct ScenarioSet {
    pub scenarios: Vec<Scenario>,
    fallback: Scenario,
    /// When set, every request's JSON (instructions + input) is appended as a
    /// line to this file — the `accept-sender-labels` suite reads it to assert
    /// the assembled child context (ticket #188).
    capture_dir: Option<std::path::PathBuf>,
}

impl Default for ScenarioSet {
    fn default() -> Self {
        Self {
            scenarios: Vec::new(),
            fallback: fallback(),
            capture_dir: None,
        }
    }
}

impl ScenarioSet {
    pub fn load_dir(dir: &Path) -> Result<Self, String> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("scenarios dir {}: {e}", dir.display()))?
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        let mut scenarios = Vec::new();
        for path in &paths {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let mut scenario: Scenario =
                serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
            if scenario.turns.is_empty() {
                return Err(format!(
                    "{}: a scenario needs at least one turn",
                    path.display()
                ));
            }
            // The counter is not deserializable; start it fresh.
            scenario.counter = AtomicUsize::new(0);
            scenarios.push(scenario);
        }
        Ok(ScenarioSet {
            scenarios,
            fallback: fallback(),
            capture_dir: std::env::var_os("TAU_MOCK_CAPTURE").map(std::path::PathBuf::from),
        })
    }

    pub const fn len(&self) -> usize {
        self.scenarios.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.scenarios.is_empty()
    }

    /// Zero every scenario's turn counter (a wdio spec retry re-runs
    /// against the same server process); returns the number reset.
    pub fn reset(&self) -> usize {
        for s in &self.scenarios {
            s.reset();
        }
        self.scenarios.len()
    }

    /// The scenario a request belongs to: the `instructions` text first
    /// (the observer/reflector live there), then the user messages in
    /// order; the fallback when nothing matches.
    pub fn select(&self, req: &WireRequest) -> &Scenario {
        if let Some(ins) = &req.instructions
            && let Some(hit) = self.scenarios.iter().find(|s| ins.contains(&s.pattern))
        {
            return hit;
        }
        for item in &req.input {
            let Some(obj) = item.as_object() else {
                continue;
            };
            if obj.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let Some(content) = obj.get("content").and_then(Value::as_str) else {
                continue;
            };
            if let Some(hit) = self.scenarios.iter().find(|s| content.contains(&s.pattern)) {
                return hit;
            }
        }
        &self.fallback
    }
    /// Append a request to the capture dir named by `TAU_MOCK_CAPTURE` (the
    /// `accept-sender-labels` suite reads `requests.jsonl` there to assert the
    /// assembled child context, ticket #188). A capture that cannot be written
    /// is reported: the crate's job is to be a trustworthy witness, and a
    /// silent miss surfaces as a missing file in a suite far from the cause.
    pub fn capture_request(&self, req: &WireRequest) {
        let Some(dir) = &self.capture_dir else { return };
        if let Err(e) = write_captured(dir, req) {
            tracing::warn!("mock capture of one request failed (dir {dir:?}): {e}");
        }
    }
}

/// The capture's one failure path: the dir, the line, the append. The errors
/// name the dir, since a relative `TAU_MOCK_CAPTURE` resolves against the
/// mock's cwd, not the suite's.
fn write_captured(dir: &Path, req: &WireRequest) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let line = serde_json::to_string(req).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("requests.jsonl"))?;
    writeln!(f, "{line}")
}

/// The system-prompt plan-file section's marker (the e2e workbench child
/// templates its `write` call against `{{plan_path}}`): kept in sync with
/// `tau_core::plan::PLAN_SECTION_ANCHOR` — the mock crate stays free of a
/// tau-core dependency, so the text is mirrored, not imported.
const PLAN_FILE_ANCHOR: &str = "Your plan lives at `";

/// The request's plan-file path (the `{{plan_path}}` substitution
/// target): `None` when the system prompt carries no plan-file section.
#[must_use]
pub fn plan_path(instructions: &Option<String>) -> Option<String> {
    let ins = instructions.as_deref()?;
    let start = ins.find(PLAN_FILE_ANCHOR)? + PLAN_FILE_ANCHOR.len();
    let rest = ins.get(start..)?;
    let end = rest.find('`')?;
    Some(rest.get(..end)?.to_owned())
}

/// The clock the frames stamp, as `created_at` (epoch seconds). The mock never
/// reads the wall clock, so a scripted run's frames are byte-identical. One
/// value for the whole stream, as the real dialect does: `created_at` is the
/// response object's creation instant, echoed on every frame that embeds it
/// (research/tps-server-timestamps.md) — so the provider's decode-window math
/// sees the same zero window a real server gives, not the omitted-timestamp
/// shape.
const MOCK_EPOCH: i64 = 1_700_000_000;

/// The `data:` payloads of one turn, in wire order: `response.created`
/// (stamped), the text split into deltas, one complete
/// `response.output_item.done` per function call, `response.completed` with
/// usage and the same stamp, `[DONE]`.
/// `{{plan_path}}` in a call's arguments is replaced with `plan_path`
/// when given.
#[must_use]
pub fn render_frames(
    turn: &Turn,
    usage: &Usage,
    seq: usize,
    plan_path: Option<&str>,
) -> Vec<String> {
    let id = format!("mock-resp-{seq}");
    let mut frames = vec![
        json!({
            "type": "response.created",
            "created_at": MOCK_EPOCH,
            "response": { "id": id, "created_at": MOCK_EPOCH }
        })
        .to_string(),
    ];
    if let Some(text) = &turn.text {
        for chunk in chunk_text(text) {
            frames
                .push(json!({ "type": "response.output_text.delta", "delta": chunk }).to_string());
        }
    }
    for (i, call) in turn.calls.iter().enumerate() {
        // `{{plan_path}}` is the one scenario placeholder: the child's plan
        // file path is request-specific (it embeds the session id), so the
        // fixture templates it and the render substitutes it.
        let args = call.arguments.to_string();
        let arguments = match plan_path {
            Some(p) => args.replace("{{plan_path}}", p),
            None => args,
        };
        frames.push(
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": format!("mock-item-{seq}-{i}"),
                    "call_id": format!("mock-call-{seq}-{i}"),
                    "name": call.name,
                    "arguments": arguments,
                }
            })
            .to_string(),
        );
    }
    frames.push(
        json!({
            "type": "response.completed",
            "response": { "id": id, "created_at": MOCK_EPOCH, "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "total_tokens": usage.total_tokens,
            } }
        })
        .to_string(),
    );
    frames.push("[DONE]".into());
    frames
}

/// Char-safe fixed-width chunks: the deltas concatenate back to the exact
/// source text (no word-boundary assumptions).
fn chunk_text(text: &str) -> Vec<String> {
    const WIDTH: usize = 32;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= WIDTH {
        return vec![text.to_owned()];
    }
    chars.chunks(WIDTH).map(|c| c.iter().collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_appends_one_line_per_request() {
        let dir = tempfile::tempdir().unwrap();
        let set = ScenarioSet {
            scenarios: Vec::new(),
            fallback: fallback(),
            capture_dir: Some(dir.path().to_path_buf()),
        };
        set.capture_request(&WireRequest {
            instructions: Some("first".into()),
            input: vec![],
        });
        set.capture_request(&WireRequest {
            instructions: None,
            input: vec![json!({"role": "user", "content": "second"})],
        });
        let raw = std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap();
        let lines: Vec<Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).expect("a captured line is JSON"))
            .collect();
        assert_eq!(lines.len(), 2, "one line per request, appended");
        assert_eq!(lines[0]["instructions"], json!("first"));
        // `instructions` has no skip_serializing_if, so the absent case rides
        // the capture as null — the witness file's shape, asserted as written.
        assert_eq!(lines[1]["instructions"], Value::Null);
        assert_eq!(lines[1]["input"][0]["content"], json!("second"));
    }

    #[test]
    fn an_uncapturable_dir_is_no_failure_for_the_served_turn() {
        // The negative: a capture that cannot be written (the dir path is a
        // file) must not break the request path — the suite's witness is
        // missing, and the warn is what names that cause.
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("taken");
        std::fs::write(&taken, b"not a dir").unwrap();
        let set = ScenarioSet {
            scenarios: Vec::new(),
            fallback: fallback(),
            capture_dir: Some(taken),
        };
        set.capture_request(&WireRequest {
            instructions: None,
            input: vec![],
        });
        assert_eq!(set.select(&WireRequest::default()).turns.len(), 1);
    }

    #[test]
    fn chunk_text_round_trips() {
        for text in [
            "short".to_string(),
            "x".repeat(32),
            "w ".repeat(50),
            "héllo wörld — char safe".to_string(),
        ] {
            let chunks = chunk_text(&text);
            let joined: String = chunks.concat();
            assert_eq!(joined, text, "chunks must concatenate exactly");
        }
    }

    #[test]
    fn every_frame_that_embeds_the_response_carries_one_created_at() {
        // research/tps-server-timestamps.md: `created_at` is the response
        // object's creation instant, echoed on every frame that embeds it. The
        // provider reads it off `response.created` (top level) and
        // `response.completed` (under `response`); a mock that stamps neither
        // exercises the omitted-timestamp decode path, never the real one.
        let frames = render_frames(
            &Turn {
                text: Some("hi".into()),
                calls: vec![],
            },
            &Usage {
                input_tokens: 1,
                output_tokens: 1,
                total_tokens: 2,
            },
            3,
            None,
        );
        let frame = |kind: &str| -> Value {
            frames
                .iter()
                .map(|f| serde_json::from_str(f).expect("a frame is JSON"))
                .find(|v: &Value| v["type"] == kind)
                .unwrap_or_else(|| panic!("no {kind} frame in the script"))
        };
        assert_eq!(
            frame("response.created")["created_at"].as_i64(),
            Some(MOCK_EPOCH)
        );
        assert_eq!(
            frame("response.completed")["response"]["created_at"].as_i64(),
            Some(MOCK_EPOCH)
        );
    }

    #[test]
    fn a_call_turn_renders_the_complete_arguments_string() {
        let turn = Turn {
            text: None,
            calls: vec![Call {
                name: "write".into(),
                arguments: json!({ "path": "a.txt", "content": "hi" }),
            }],
        };
        let frames = render_frames(&turn, &Usage::default(), 0, None);
        let done = frames
            .iter()
            .find(|f| f.contains("\"type\":\"response.output_item.done\""))
            .expect("the output_item.done frame");
        let v: Value = serde_json::from_str(done).unwrap();
        let item = &v["item"];
        assert_eq!(item["name"], "write");
        // The wire contract: arguments is a JSON *string*, whole in one
        // event (the client does not reassemble argument fragments).
        assert!(item["arguments"].is_string());
        assert_eq!(
            serde_json::from_str::<Value>(item["arguments"].as_str().unwrap()).unwrap(),
            json!({ "path": "a.txt", "content": "hi" })
        );
        assert_eq!(frames.last().unwrap(), "[DONE]");
    }

    #[test]
    fn plan_path_is_extracted_from_the_prompt_section() {
        let ins = "be terse\n\n# Plan file\n\nYour plan lives at `/w/.tau/plans/abc123.md` — edit it as you go.\n".to_owned();
        assert_eq!(
            plan_path(&Some(ins)),
            Some("/w/.tau/plans/abc123.md".to_owned())
        );
        // No plan-file section: nothing to substitute.
        assert_eq!(plan_path(&Some("be terse".to_owned())), None);
        assert_eq!(plan_path(&None), None);
    }

    #[test]
    fn plan_file_anchor_stays_in_sync_with_core() {
        // The mock mirrors the core's plan-section marker without a compile
        // dependency (it is a standalone test server); if the core's wording
        // drifts, `{{plan_path}}` substitution silently no-ops and the
        // workbench e2e fails far from the cause. Read the core source as
        // text so any drift fails here, next to the cause.
        let core =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tau-core/src/plan/mod.rs");
        let src = std::fs::read_to_string(&core)
            .unwrap_or_else(|e| panic!("read {}: {e}", core.display()));
        assert!(
            src.contains(PLAN_FILE_ANCHOR),
            "PLAN_FILE_ANCHOR {PLAN_FILE_ANCHOR:?} no longer appears in plan.rs — the plan-section wording drifted, update the mock's mirrored anchor"
        );
    }

    #[test]
    fn render_substitutes_the_plan_path_placeholder() {
        let turn = Turn {
            text: None,
            calls: vec![Call {
                name: "write".into(),
                arguments: json!({ "path": "{{plan_path}}", "content": "# Plan" }),
            }],
        };
        let frames = render_frames(&turn, &Usage::default(), 0, Some("/w/.tau/plans/abc.md"));
        let done = frames
            .iter()
            .find(|f| f.contains("\"type\":\"response.output_item.done\""))
            .expect("the output_item.done frame");
        let v: Value = serde_json::from_str(done).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(v["item"]["arguments"].as_str().unwrap()).unwrap(),
            json!({ "path": "/w/.tau/plans/abc.md", "content": "# Plan" })
        );
    }

    #[test]
    fn selection_prefers_instructions_over_messages() {
        let mut set = ScenarioSet::default();
        set.scenarios.push(Scenario {
            pattern: "observer-marker".into(),
            turns: vec![Turn {
                text: Some("obs".into()),
                calls: vec![],
            }],
            usage: Usage::default(),
            counter: AtomicUsize::new(0),
        });
        let req = WireRequest {
            instructions: Some("you are the X observer-marker Y".into()),
            input: vec![json!({ "role": "user", "content": "body" })],
        };
        assert_eq!(set.select(&req).pattern, "observer-marker");
        // No marker anywhere: the fallback, not an error.
        let plain = WireRequest {
            instructions: None,
            input: vec![json!({ "role": "user", "content": "hello" })],
        };
        assert_eq!(set.select(&plain).pattern, "");
    }

    #[test]
    fn the_turn_counter_clamps_at_the_end_of_the_script() {
        let s = Scenario {
            pattern: "x".into(),
            turns: vec![
                Turn {
                    text: Some("one".into()),
                    calls: vec![],
                },
                Turn {
                    text: Some("two".into()),
                    calls: vec![],
                },
            ],
            usage: Usage::default(),
            counter: AtomicUsize::new(0),
        };
        assert_eq!(s.next_turn().1.text.as_deref(), Some("one"));
        assert_eq!(s.next_turn().1.text.as_deref(), Some("two"));
        assert_eq!(s.next_turn().1.text.as_deref(), Some("two"));
    }

    #[test]
    fn reset_replays_the_script_from_the_top() {
        let s = Scenario {
            pattern: "x".into(),
            turns: vec![
                Turn {
                    text: Some("one".into()),
                    calls: vec![],
                },
                Turn {
                    text: Some("two".into()),
                    calls: vec![],
                },
            ],
            usage: Usage::default(),
            counter: AtomicUsize::new(0),
        };
        assert_eq!(s.next_turn().1.text.as_deref(), Some("one"));
        assert_eq!(s.next_turn().1.text.as_deref(), Some("two"));
        assert_eq!(s.next_turn().1.text.as_deref(), Some("two"));
        s.reset();
        assert_eq!(s.next_turn().1.text.as_deref(), Some("one"));
    }
}
