use super::assemble::{Assembled, input_items};
use crate::provider::{InputEntry, ToolSpec};

/// What the provider charged for one request the turn sent, held beside the
/// parts of the assembly it priced: the estimate for the next call anchors on
/// that count, so only material appended after the anchored request is ever a
/// guess (spec §4, plan §6.4). The guard fields carry the identity of what the
/// count describes, so a stale anchor fails at the read rather than silently
/// pricing a different prompt (plan §6.8). Each guard is an xxh3 digest of the
/// content it names — the content stays the rule (a reflector rewriting the
/// log moves the digest), and the anchor carries no copy of the prompt.
#[derive(Debug, Clone)]
pub(super) struct PromptAnchor {
    /// The model that answered, whose tokenizer produced the count. A session's
    /// model can change while a turn runs.
    model: String,
    /// The entry the anchored request ended at.
    leaf: String,
    /// The instructions it carried: the plan state rides here.
    instructions: u64,
    /// The tool specs it carried, digested from the same serialization the
    /// whole-prompt estimate prices.
    tools: u64,
    /// The observation items it carried, as the assembly digested them. The log
    /// rides the request's items, not its instructions, and a reflector can
    /// rewrite it without adding or removing an item, so the content is the
    /// guard. `None` — a prefix the serializer refused — is no anchor: the
    /// whole-prompt estimate is always correct, so declining is free.
    om_digest: Option<u64>,
    /// The provider's own `usage.input_tokens` — never an estimate (plan §6.4:
    /// the provider's number is the recorded number).
    input_tokens: u64,
}

impl PromptAnchor {
    /// The anchor for the request just sent, charged `input_tokens` by the
    /// provider. None for an assembly with no entries to end at (a session
    /// whose first call has no history yet).
    pub(super) fn after_call(
        assembled: &Assembled,
        tools_json: &str,
        input_tokens: u64,
    ) -> Option<Self> {
        Some(Self {
            model: assembled.model.clone(),
            leaf: assembled.leaf()?.to_owned(),
            instructions: digest(&assembled.instructions),
            tools: digest(tools_json),
            om_digest: assembled.om_digest,
            input_tokens,
        })
    }

    /// This assembly priced from the anchor: the provider's count plus the
    /// estimate for the entries appended since. None when this is not that
    /// request with a tail — a model switch, a compaction, a re-injection, a
    /// changed tool surface, or a branch that no longer carries the entry it
    /// ended at.
    /// `None` when the observation prefix has no digest: nothing shows the
    /// anchor describes that prompt, so it declines rather than pricing one it
    /// cannot identify.
    fn price(&self, assembled: &Assembled, tools_json: &str) -> Option<u32> {
        let om_digest = assembled.om_digest?;
        if self.model != assembled.model
            || self.instructions != digest(&assembled.instructions)
            || self.tools != digest(tools_json)
            || self.om_digest != Some(om_digest)
        {
            return None;
        }
        let tail = assembled.tail_after(&self.leaf)?;
        let items = input_items(tail, assembled.parent_session.as_deref());
        let delta = u64::from(crate::provider::items_token_estimate(&items));
        // A count beyond u32::MAX describes a prompt over any declared window,
        // so it reads as the ceiling and the gate refuses. Falling back to the
        // whole-prompt estimate here would under-state the one prompt that most
        // needs refusing (plan §6.4's failure mode).
        Some(u32::try_from(self.input_tokens.saturating_add(delta)).unwrap_or(u32::MAX))
    }
}

/// The tool specs as the request serializes them — computed once per call so
/// the anchor digests the same string the whole-prompt estimate prices.
pub(super) fn tool_fingerprint(tools: &[ToolSpec]) -> String {
    serde_json::to_string(tools).unwrap_or_default()
}

/// The digest of the items the observation state contributed, taken at
/// assembly where those items are built: pricing a prompt costs a comparison
/// and never a re-serialization of the log. An empty slice is the no-OM
/// assembly's zero contribution, digested like any other. A serialization
/// failure has no digest — the caller prices the whole prompt, the number that
/// is always right.
pub(super) fn om_items_digest(items: &[InputEntry]) -> Option<u64> {
    serde_json::to_string(items)
        .ok()
        .map(|json| xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
}

fn digest(value: &str) -> u64 {
    xxhash_rust::xxh3::xxh3_64(value.as_bytes())
}

/// The prompt's size for the gate and the output clamp: anchored when an anchor
/// covers this assembly, otherwise the whole-prompt estimate — a turn's first
/// call, or any call after the prompt changed under the anchor (plan §6.4).
pub(super) fn prompt_size(
    anchor: Option<&PromptAnchor>,
    assembled: &Assembled,
    tools_json: &str,
) -> u32 {
    anchor
        .and_then(|a| a.price(assembled, tools_json))
        .unwrap_or_else(|| {
            crate::provider::prompt_token_estimate(
                &assembled.instructions,
                &assembled.items,
                tools_json,
            )
        })
}
