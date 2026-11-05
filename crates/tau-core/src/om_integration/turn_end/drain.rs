//! The drain valve (#409): at the settle, a window past the drain line is
//! caught up in bounded slices, in the open. Lag blocks the settle, never
//! the turn.

use super::super::{OmError, OmState, SessionStore};
use super::TurnEndAction;
use crate::provider::TurnProviderRef;

/// The drain line as a percentage of the *effective* observe threshold
/// (#409): 120%, mastra's `blockAfter: 1.2` parity. Past it, "catch up
/// when idle" stops being a schedule and becomes lag, so the settle
/// drains the backlog itself.
const DRAIN_AFTER_PERCENT: u64 = 120;

/// The per-settle cap on drain slices (#409). Each slice is
/// threshold-sized, so eight covers any catch-up a turn can produce in a
/// session that honors its interval; the cap is the cost bound for
/// degenerate folds — a threshold so small the slices are entry-sized —
/// where an uncapped drain would pay one call per entry inside one
/// settle. The remainder drains at the next settle: the window is still
/// past the line, and the next settle's gate re-reads it.
const MAX_DRAIN_SLICES: u32 = 8;

impl OmState {
    /// The drain line: `DRAIN_AFTER_PERCENT` of the session's effective
    /// observe threshold — the same ramped number the plan's activation
    /// and the slice bound read, so line and plan ride one quantity.
    pub(in crate::om_integration::turn_end) fn drain_line(&self) -> u32 {
        let threshold = u64::from(
            self.config
                .observe_threshold_for(self.record.observation_tokens),
        );
        u32::try_from(threshold * DRAIN_AFTER_PERCENT / 100).unwrap_or(u32::MAX)
    }

    /// Run the settled plan, then drain if the window sits at or past the
    /// line. Each slice plans from live state: the window is re-read under
    /// the session lock, and the buffered chunks are the record's — a cycle
    /// commits through the session's own store, whose write-back mirror
    /// (`commit_chunk`) lands them in this state's record under the same
    /// lock, so the plan's cut sees them (#402's discipline, N-fold).
    ///
    /// A slice that advances nothing — a degenerate answer — ends the
    /// drain: the settle never pays twice for one frozen span. A failed
    /// slice returns `Err` with everything already paid committed; the
    /// agent latches `om_failed` once and the turn proceeds (#86).
    /// The gate quantity is the whole raw window past the observation
    /// cursor; the plan's is the slice past BOTH cursors — raw the log
    /// already carries in a chunk is not pending work. The promote, which
    /// runs before the gate, closes that gap: chunk-covered raw becomes
    /// observations free, and what stays chunk-covered is exactly what the
    /// plan refuses to re-show.
    pub(in crate::om_integration::turn_end) async fn run_and_drain<W>(
        &mut self,
        with_store: &mut W,
        provider: &TurnProviderRef,
        model: &str,
        hook: Option<&crate::agent::OmStatusHook>,
        action: &mut TurnEndAction,
    ) -> Result<(), OmError>
    where
        W: FnMut(&mut dyn FnMut(&mut SessionStore) -> Result<(), OmError>) -> Result<(), OmError>,
    {
        let draining = self.record.pending_tokens >= self.drain_line();
        let cursor_before = self.record.cursor.clone();
        self.run_actions(with_store, provider, model, hook, action)
            .await?;
        if !draining || self.record.cursor == cursor_before {
            return Ok(());
        }
        let mut slices = 0u32;
        loop {
            let mut unobserved: Option<Vec<crate::session::Entry>> = None;
            with_store(&mut |store| {
                unobserved = Some(self.unobserved(store)?);
                self.fast_forward_chunks_from_durable(store)?;
                Ok(())
            })?;
            let unobserved = unobserved.expect("the phase ran");
            self.record.pending_tokens = self.pending_tokens(&unobserved);
            if self.record.pending_tokens < self.drain_line() {
                break;
            }
            let cursor_before = self.record.cursor.clone();
            let mut slice = self.plan(&unobserved);
            if matches!(slice, TurnEndAction::Done) {
                break;
            }
            self.run_actions(with_store, provider, model, hook, &mut slice)
                .await?;
            slices += 1;
            if self.record.cursor == cursor_before || slices >= MAX_DRAIN_SLICES {
                break;
            }
        }
        Ok(())
    }

    /// The drain slice's read of the durable chunk state (D4: the file wins
    /// over any snapshot). A background cycle commits through the session's
    /// own store while the pass runs on its own copy of the state, so between
    /// slices the file's chunk fields are ahead of this record's, and a slice
    /// that planned over a cycle's missing chunk re-observed raw the new chunk
    /// stands for (#402's discipline, N-fold). The buffer cursor advances to
    /// the durable chunks' furthest end and never retreats: a promote that
    /// drained every chunk leaves this state's cursor naming the range it
    /// covered, and resetting it to that empty set would cut the plan's window
    /// at the branch head — the raw the promoted text summarizes, twice.
    fn fast_forward_chunks_from_durable(
        &mut self,
        store: &mut SessionStore,
    ) -> Result<(), OmError> {
        let durable = OmState::load_record(store)?;
        self.record.buffered_chunks = durable.buffered_chunks;
        self.record.last_buffered_at_tokens = self
            .record
            .last_buffered_at_tokens
            .max(durable.last_buffered_at_tokens);
        if let Some(end) = super::super::last_chunk_end(&self.record.buffered_chunks) {
            let advanced = match self.buffer_cursor.as_deref() {
                None => true,
                // A non-numeric cursor (a record from a build that minted
                // differently) stands: the durable end cannot order it.
                Some(seen) => match (seen.parse::<u64>(), end.parse::<u64>()) {
                    (Ok(seen), Ok(end)) => end > seen,
                    _ => false,
                },
            };
            if advanced {
                self.buffer_cursor = Some(end);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::om::OmConfig;

    fn state(observe_threshold: u64, observation_tokens: u32) -> OmState {
        OmState {
            record: crate::om::OmRecord {
                observation_tokens,
                ..Default::default()
            },
            config: OmConfig {
                observe_threshold: u32::try_from(observe_threshold).unwrap(),
                ..Default::default()
            },
            buffer_cursor: None,
        }
    }

    #[test]
    fn the_drain_line_is_the_ramped_threshold_at_120_percent() {
        // The line rides the same effective threshold the plan gates on:
        // 30k base, no observations, and the share-budget ramp both land.
        assert_eq!(state(30_000, 0).drain_line(), 36_000);
        assert_eq!(state(1_000, 0).drain_line(), 1_200);
        // The ramp: with observations carrying the budget, the effective
        // threshold grows and the line grows with it.
        let mut ramped = state(30_000, 20_000);
        ramped.config.share_token_budget = true;
        ramped.config.reflect_threshold = 40_000;
        // effective = max(observe, observe + reflect - observed) = 50k.
        assert_eq!(ramped.drain_line(), 60_000);
    }

    #[test]
    fn the_drain_line_never_overflows_the_token_domain() {
        // A threshold at the u32 ceiling must not wrap the line to a small
        // number: the line saturates.
        assert_eq!(state(u64::from(u32::MAX), 0).drain_line(), u32::MAX);
    }

    /// The chunk the durable record carries, for the drain slice's read.
    fn chunk(first: &str, last: &str, text: &str) -> crate::om_integration::BufferedChunk {
        crate::om_integration::BufferedChunk {
            range: (first.into(), last.into()),
            last_ts: 100,
            text: text.into(),
            tokens: 100,
        }
    }

    #[test]
    fn a_drain_slice_takes_the_durable_chunks_as_the_ones_it_has() {
        // The record is the chunk store (ticket #86 P2): a background cycle
        // commits through the session's store while the pass runs on its own
        // copy of the state, so between slices the file is ahead. The slice
        // plans over the durable set — a second copy kept in sync by hand is
        // the state space a mirror does not share.
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::super::super::tests::store_with_text_entries(dir.path(), 2, 100);
        let durable = crate::om::OmRecord {
            buffered_chunks: vec![chunk("00000001", "00000002", "cycle chunk")],
            last_buffered_at_tokens: 500,
            ..Default::default()
        };
        OmState::from_config(&crate::config::Om::default(), durable)
            .save(&mut store)
            .unwrap();
        // The pass's copy: the chunks a concurrent promote drained, so its
        // indices are not the durable set's.
        let mut live = state(30_000, 0);
        live.record.buffered_chunks = vec![
            chunk("00000001", "00000002", "cycle chunk"),
            chunk("00000003", "00000004", "already promoted"),
        ];

        live.fast_forward_chunks_from_durable(&mut store).unwrap();

        assert_eq!(
            live.buffered()
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            vec!["cycle chunk"],
            "the slice's chunks are the file's, not the copy's"
        );
        assert_eq!(live.buffer_cursor.as_deref(), Some("00000002"));
        assert_eq!(live.record.last_buffered_at_tokens, 500);
    }

    #[test]
    fn a_slice_does_not_cut_the_plan_at_the_branch_head_after_a_promote() {
        // The promote drains every chunk and latches the cursor on the range
        // it covered. The durable record then holds no chunks at all: a slice
        // that rebuilt the cursor from the empty durable set cut the plan's
        // window at the branch head, and the next Observer pass was shown the
        // raw the promoted text summarizes (#408 pays once).
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::super::super::tests::store_with_text_entries(dir.path(), 2, 100);
        let mut live = state(30_000, 0);
        live.buffer_cursor = Some("00000006".into());

        live.fast_forward_chunks_from_durable(&mut store).unwrap();

        assert!(live.buffered().is_empty(), "no chunk is durable");
        assert_eq!(
            live.buffer_cursor.as_deref(),
            Some("00000006"),
            "the cursor stands: the durable set is empty, not behind it"
        );
    }
}
