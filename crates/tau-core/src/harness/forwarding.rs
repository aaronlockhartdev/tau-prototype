//! The streaming seam: the forwarding provider that mirrors stream events onto the protocol channel, the protocol mapping, the session provider factory, and the sub-agent factory/driver/bridge.

use super::{
    AgentSession, Arc, AtomicBool, AtomicU64, BoxedDrive, ChildDriver, ChildProviderFactory,
    Config, ContextMode, Core, Duration, Event, LiveSession, Mutex, Ordering, PipeCounters,
    ProviderTurn, ResponseRequest, SessionMeta, SkillInfo, SpawnNotice, StateNotice,
    SubagentBridge, SubagentEventKind, SubagentInfo, TurnEvent, TurnProvider, TurnProviderRef,
    TurnSink, Usage, WakeKind, WakeNotice, Weak, mpsc, pipe_send, provider, run_turn,
};
use crate::config::ProviderRef;
use std::future::Future;
use std::pin::Pin;
/// A `TurnProvider` wrapper that mirrors stream events onto the protocol
/// channel (spec §8 streaming) before the loop's own sink sees them.
pub(crate) struct ForwardingProvider {
    /// The endpoint this session's calls go to (#336). The constructor's
    /// provider is the first of them; a model switch to another provider's
    /// model rebinds it, so the next turn's call answers to the model the
    /// session carries, not the provider the config's map sorts first.
    route: Mutex<TurnProviderRef>,
    pub(crate) tx: mpsc::Sender<Event>,
    /// The core's pipe drop bookkeeping (shared): an overflow in a child
    /// stream is counted and surfaced exactly like the core's own pipe.
    pub(crate) pipe: PipeCounters,
    pub(crate) workspace: String,
    pub(crate) session: String,
    pub(crate) call_seq: AtomicU64,
    /// This session's call ledger: one outcome per call it starts, and the
    /// one stop flag. It crosses back to the live session through
    /// `TurnProvider::call_log` — the provider the agent calls and the
    /// provider the reconciliation reads are one object (#175).
    log: provider::CallLog,
}

impl ForwardingProvider {
    /// The six live dependencies, named; the per-provider call ledger is
    /// always fresh.
    pub(crate) fn new(
        inner: TurnProviderRef,
        tx: mpsc::Sender<Event>,
        pipe: PipeCounters,
        workspace: String,
        session: String,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            route: Mutex::new(inner),
            tx,
            pipe,
            workspace,
            session,
            call_seq: AtomicU64::new(0),
            log: provider::CallLog::new(stop),
        }
    }
}

impl TurnProvider for ForwardingProvider {
    fn call_log(&self) -> provider::CallLog {
        self.log.clone()
    }

    /// A model switch to another provider's model rebinds the endpoint here.
    /// It takes effect at the next call: an in-flight stream keeps the endpoint
    /// it started on, so the switch lands at the turn boundary (#336).
    fn set_route(&self, inner: TurnProviderRef) {
        *self.route.lock() = inner;
    }

    fn forwarding_inner(&self) -> Option<TurnProviderRef> {
        Some(self.route.lock().clone())
    }

    /// The `[requests]` move rides through the wrapper to the endpoint behind
    /// it (#347): a live app's session provider is this wrapper, so the
    /// boundary's write lands on the transport that runs the next call. A
    /// route rebind builds its endpoint from the live section (`session_inner`
    /// at the switch), so the move survives the switch too.
    fn set_requests(&self, requests: &crate::config::Requests) {
        self.route.lock().set_requests(requests);
    }
    fn call<'a>(&self, request: &ResponseRequest, sink: &'a mut dyn TurnSink) -> ProviderTurn<'a> {
        let call_id = format!(
            "{}-{:08}",
            self.session,
            self.call_seq.fetch_add(1, Ordering::Relaxed) + 1
        );
        // The sink is fully owned (clones of the shared state), so the
        // future captures no lifetimes of its own beyond the loop's sink.
        let mut forward = ForwardSink {
            tx: self.tx.clone(),
            pipe: self.pipe.clone(),
            workspace: self.workspace.clone(),
            session: self.session.clone(),
            log: self.log.clone(),
            call_id,
            inner: sink,
        };
        // Record the call at start, not first event (#67): a call aborted
        // before it emits anything is still a call, and must finalize like
        // any other — a StreamStart now, a StreamEnd at turn settlement.
        forward.log.start(&forward.call_id);
        forward.send(Event::StreamStart {
            workspace: forward.workspace.clone(),
            session: forward.session.clone(),
            call_id: forward.call_id.clone(),
        });
        let inner = self.route.lock().clone();
        let request = request.clone();
        let log = self.log.clone();
        let settled = forward.call_id.clone();
        Box::pin(async move {
            let result = inner.call(&request, &mut forward).await;
            // The call resolved — cleanly, or as the provider error the loop
            // goes on to handle: its end is now the settlement's to take. A
            // task torn down mid-await never runs this line, so its end stays
            // open and the next settlement closes it as an interruption.
            log.settle(&settled);
            result
        })
    }
}

/// One stream's mirror: forwards each delta onto the protocol channel and
/// delegates to the loop's own sink (the kill point stays the loop's).
pub(crate) struct ForwardSink<'a> {
    pub(crate) tx: mpsc::Sender<Event>,
    pipe: PipeCounters,
    pub(crate) workspace: String,
    session: String,
    log: provider::CallLog,
    call_id: String,
    inner: &'a mut dyn TurnSink,
}

impl TurnSink for ForwardSink<'_> {
    fn event(&mut self, event: TurnEvent) -> bool {
        // Text/reasoning deltas are the loop's sink's: they become the
        // growing entry's snapshots (ADR-0008), not channel events.
        // No `StreamEnd` here: the Completed frame ends the call's stream,
        // not the turn — the turn's om_turn_end pass still runs after it.
        // The end arrives with the post-turn reconciliation, taken from the
        // call's ledger entry (issue #82).
        // A stop cuts the stream at the next delta (the loop records the
        // partial as interrupted, spec §7).
        if self.log.stop.load(Ordering::SeqCst) {
            return false;
        }
        // The ledger gains the Completed frame's usage and duration exactly
        // when the loop's sink accepts them — the same moment they reach the
        // assistant entry, so the event and the record never disagree (6B). A
        // refused frame reaches neither, which is the force-kill race: the
        // frame lands on a stream the loop has already stopped reading.
        let completed = match &event {
            TurnEvent::Completed(usage, duration) => {
                Some((usage.clone(), duration.map(|d| d.as_secs_f64())))
            }
            _ => None,
        };
        let accepted = self.inner.event(event);
        if accepted && let Some((usage, duration_secs)) = completed {
            self.log.complete(&self.call_id, usage, duration_secs);
        }
        accepted
    }
    fn stop_signal(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(stop_flag(&self.log.stop))
    }
}

/// The prefill half of a stop (spec §7): a poll over the flag so the
/// provider tears the in-flight request down before the first stream event
/// arrives. 50 ms: a human clicking stop, not a tight race.
async fn stop_flag(stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl ForwardSink<'_> {
    fn send(&self, event: Event) {
        // A drop is never silent (the shared counter + a one-shot summary error)
        pipe_send(
            &self.tx,
            &self.pipe,
            event,
            self.workspace.clone(),
            Some(self.session.clone()),
        );
    }
}

/// The context modes are structurally identical (both three-lowercase);
/// the mapping is the ADR-0002 crate boundary.
pub(crate) const fn mode_to_protocol(m: crate::subagent::ContextMode) -> ContextMode {
    match m {
        crate::subagent::ContextMode::Fresh => ContextMode::Fresh,
        crate::subagent::ContextMode::Compacted => ContextMode::Compacted,
        crate::subagent::ContextMode::Fork => ContextMode::Fork,
    }
}

/// The core's child record as the protocol's (the protocol crate is
/// independent of the core — ADR-0002 — so the mapping lives here; the
/// state's `From` is in `subagent::states`, its one owner).
pub(crate) fn info_to_protocol(i: &crate::subagent::SubagentInfo) -> SubagentInfo {
    SubagentInfo {
        handle: i.handle.clone(),
        child: i.child.clone(),
        agent_type: i.agent_type.clone(),
        context_mode: mode_to_protocol(i.context_mode),
        state: i.state.clone().into(),
        last_message: i.last_message.clone(),
        created: i.created,
        usage: i.usage.as_ref().map(|u| Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_prompt_tokens: u.cached_input_tokens,
        }),
        resume_contract: i.resume_contract.clone(),
    }
}

/// A session's provider: a `canned://` entry (the dev-gated test hook) takes
/// its scripted replay; everything else is the live endpoint. A `canned://`
/// entry in a release build has no scripts, so it falls through to the live
/// endpoint, where the unresolvable URL fails the turn.
pub(crate) fn session_inner(
    client: &reqwest::Client,
    p: &crate::config::Provider,
    requests: &crate::config::Requests,
) -> TurnProviderRef {
    provider::canned_dev(p).unwrap_or_else(|| provider::production(client, p, requests))
}

/// The endpoint the memory passes are addressed at, from `[om].om_model` and
/// §6.7 (#348): the provider its id names, asked for the key that provider
/// declares. `current` is the route the session (or child) itself is on — the
/// answer for a bare key several providers declare, and only for that case: an
/// id that resolves nowhere is a refusal carrying the reason the session will
/// record, never the session's own address borrowed.
pub(crate) fn om_route_for(
    client: &reqwest::Client,
    config: &Config,
    om_model: &str,
    current: ProviderRef<'_>,
) -> crate::agent::OmRoute {
    match config.route_om(om_model, current) {
        Ok(owner) => crate::agent::OmRoute::Configured {
            // Plain, never the forwarding seam: an OM call is not a user-facing
            // stream (ticket #86).
            provider: session_inner(client, owner.provider, &config.requests),
            model: Config::model_key_for(owner.name, om_model).to_owned(),
        },
        Err(miss) => crate::agent::OmRoute::Refused {
            reason: miss.to_string(),
        },
    }
}

/// The child provider factory (ticket #23 N3): wraps the provider the parent's
/// model is addressed at in the forwarding seam, per child session id.
pub(crate) struct ForwardingChildFactory {
    pub(crate) client: reqwest::Client,
    /// The workspace config: a `.md` agent type names a model of its own, and
    /// that id routes to its own provider (#336) — the parent's entry is only
    /// the fallback a bare key resolves against.
    pub(crate) config: Config,
    pub(crate) provider: crate::config::Provider,
    /// The config key of `provider`: §6.7 answers a contested bare key against a
    /// provider's name, and the factory carries the parent's entry (#348).
    pub(crate) provider_name: String,
    pub(crate) requests: crate::config::Requests,
    pub(crate) tx: mpsc::Sender<Event>,
    pub(crate) pipe: PipeCounters,
    pub(crate) workspace: String,
}
impl ChildProviderFactory for ForwardingChildFactory {
    fn create(&self, child: &str, model: &str, stop: &Arc<AtomicBool>) -> TurnProviderRef {
        // #336: a child is addressed at the provider its model's id names (a
        // `.md` agent type's model, or the parent's), not at the provider the
        // parent happened to open on. A prefix naming no provider is no route
        // — the child keeps the parent's provider, whose id the spawn already
        // resolved.
        let owner = self.config.route_child(
            model,
            ProviderRef {
                name: &self.provider_name,
                provider: &self.provider,
            },
        );
        Arc::new(ForwardingProvider::new(
            session_inner(&self.client, owner, &self.requests),
            self.tx.clone(),
            self.pipe.clone(),
            self.workspace.clone(),
            child.to_owned(),
            stop.clone(),
        ))
    }
}

/// The child driver (N3): a child's in-flight round is a plain session
/// turn — the child is an ordinary session, so its drive is `run_turn` on
/// its live session (registered by the bridge at spawn).
pub(crate) struct TurnChildDriver {
    pub(crate) core: Weak<Core>,
}
impl ChildDriver for TurnChildDriver {
    fn drive(&self, session: &str, _agent: &Arc<AgentSession>) -> BoxedDrive {
        let live = self
            .core
            .upgrade()
            .and_then(|c| c.sessions.lock().get(session).cloned());
        let Some((core, live)) = self.core.upgrade().zip(live) else {
            return Box::pin(async { Ok(()) });
        };
        Box::pin(async move {
            run_turn(core, live).await;
            Ok(())
        })
    }
}

/// The dispatch-side sub-agent bridge (N3): events, parent wakes, and
/// child-session registration — a child is an ordinary live session
/// (ADR-0006), so the GUI can open it the same way as any session.
pub(crate) struct SessionSubagentBridge {
    pub(crate) core: Arc<Core>,
    pub(crate) workspace: String,
}
impl SubagentBridge for SessionSubagentBridge {
    fn spawned(&self, n: &SpawnNotice) {
        // The spawn minted the title into the child's header and carried it on
        // the notice, so the GUI's stub gets the real name.
        self.core.emit(Event::SubagentEvent {
            workspace: self.workspace.clone(),
            session: n.parent.clone(),
            kind: SubagentEventKind::Spawned {
                handle: n.handle.clone(),
                child: n.child.clone(),
                agent_type: n.agent_type.clone(),
                context_mode: mode_to_protocol(n.context_mode),
                title: n.title.clone(),
            },
        });
        // Wire the child's event hooks (#173): the child launch path only wires
        // the plan hook, so its streaming entries never reached the GUI.
        let blob_root = n.cwd.join(".tau");
        super::launch::wire_events(&self.core, &self.workspace, &n.child, &n.agent, &blob_root);
        // The child is registered with the provider its agent calls through —
        // the same object the spawn handed over, built with the agent's stop
        // flag. One provider per child, as the root has (#175): the post-turn
        // reconciliation reads the calls the agent made from it, so the child's
        // `StreamEnd` carries the child's own call ids.
        let live = Arc::new(LiveSession {
            meta: Mutex::new(SessionMeta {
                id: n.child.clone(),
                workspace: self.workspace.clone(),
                title: Some(n.title.clone()),
                parent: Some(n.parent.clone()),
                created: n.created,
                leaf: None,
                model: Some(n.model.clone()),
                usage: None,
                archived: false,
            }),
            agent: n.agent.clone(),
            turn: AtomicBool::new(false),
            provider: n.provider.clone(),
            cwd: n.cwd.clone(),
        });
        let id = live.meta.lock().id.clone();
        self.core.sessions.lock().insert(id, live);
    }

    fn state(&self, n: &StateNotice) {
        // Build a `SubagentState::Stopped` with the `resume_contract` from
        // `StateNotice` (the `From` impl sets `resume_contract: None`).
        let state = match &n.state {
            crate::subagent::ChildState::Stopped { .. } => {
                let mut s: super::SubagentState = n.state.clone().into();
                if let super::SubagentState::Stopped {
                    resume_contract, ..
                } = &mut s
                {
                    resume_contract.clone_from(&n.resume_contract);
                }
                s
            }
            _ => n.state.clone().into(),
        };
        self.core.emit(Event::SubagentEvent {
            workspace: self.workspace.clone(),
            session: n.parent.clone(),
            kind: SubagentEventKind::State {
                handle: n.handle.clone(),
                child: n.child.clone(),
                state,
                note: n.note.clone(),
            },
        });
    }

    fn wake(&self, n: &WakeNotice) {
        self.core.emit(Event::SubagentEvent {
            workspace: self.workspace.clone(),
            session: n.parent.clone(),
            kind: SubagentEventKind::Notified {
                child: n.child.clone(),
                wake: match n.kind {
                    WakeKind::Done => "done",
                    WakeKind::Failed => "failed",
                    WakeKind::Waiting => "waiting",
                    WakeKind::Stopped => "stopped",
                }
                .into(),
                text: n.text.clone(),
                output: n.output.clone(),
                waiting_on: n.waiting_on.map(Into::into),
            },
        });
        // Wake the parent (ADR-0001 wake rules): the notification is already
        // on its active branch; deliver it to the loop and start a turn.
        // The report rides the steering lane — a child that finishes mid
        // parent-turn is delivered at this turn's next LLM call, not
        // spliced into a later, unrelated one.
        let Some(live) = self.core.sessions.lock().get(&n.parent).cloned() else {
            return;
        };
        let text = match &n.output {
            Some(o) => format!(
                "{} — {}",
                n.text,
                serde_json::to_string(o).unwrap_or_default()
            ),
            None => n.text.clone(),
        };
        live.agent.send_notified_steer(text, n.child.clone());
        // The Queue event is a projection of the agent's lane queue (the
        // single source of truth), taken after the wake lands in it.
        self.core.emit(Event::Queue {
            workspace: self.workspace.clone(),
            session: n.parent.clone(),
            items: live.agent.queued_items(),
        });
        live.agent.stop_flag().store(false, Ordering::SeqCst);
        // The bridge owns the core (the `Arc` it was built with): a wake's
        // turn start is never skipped, so a parked parent is always woken.
        if live
            .turn
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tokio::spawn(run_turn(Arc::clone(&self.core), live));
        }
    }
}

/// The protocol's mirror of a discovered skill (the location as a string,
/// the GUI never sees host paths as `Path`).
pub(crate) fn skill_info(s: &crate::skills::Skill) -> SkillInfo {
    SkillInfo {
        name: s.name.clone(),
        description: s.description.clone(),
        location: s.location.to_string_lossy().into_owned(),
        model_invocation: s.model_invocation,
    }
}
