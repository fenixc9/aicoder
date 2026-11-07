use super::{RunSession, SessionEntryData, SessionMetadata, SessionRepository, TurnRecord};
use crate::{
    RunJournal, RunOutcome,
    model::{Message, MessageRole},
    run::{RunFailure, RunStage},
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::{collections::HashMap, sync::Mutex};

pub(crate) struct SessionJournal<'a> {
    repository: &'a dyn SessionRepository,
    turn_id: Option<String>,
    owner: uuid::Uuid,
    state: Mutex<State>,
}
struct BoundaryPlan {
    owner: uuid::Uuid,
    plan: super::SessionAppendPlan,
}
struct State {
    session: RunSession,
    saved: HashMap<usize, String>,
    failure: Option<RunFailure>,
    finished: bool,
    initial_ids: Vec<String>,
    initial_message_count: usize,
}
impl<'a> SessionJournal<'a> {
    pub fn new(
        repository: &'a dyn SessionRepository,
        session: RunSession,
        turn_id: String,
    ) -> Self {
        Self::with_turn(repository, session, Some(turn_id))
    }
    pub(crate) fn idle(repository: &'a dyn SessionRepository, session: RunSession) -> Self {
        Self::with_turn(repository, session, None)
    }
    fn with_turn(
        repository: &'a dyn SessionRepository,
        session: RunSession,
        turn_id: Option<String>,
    ) -> Self {
        let initial_ids = session
            .messages()
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        let initial_message_count = initial_ids.len();
        Self {
            repository,
            turn_id,
            owner: uuid::Uuid::new_v4(),
            state: Mutex::new(State {
                session,
                saved: HashMap::new(),
                failure: None,
                finished: false,
                initial_ids,
                initial_message_count,
            }),
        }
    }
    pub(crate) fn branch_accounting(&self) -> Result<super::BranchAccounting> {
        self.state
            .lock()
            .expect("session journal poisoned")
            .session
            .accounting()
    }
    pub(crate) fn settle_idle_auxiliary(
        &self,
        attempts: &[crate::auxiliary::AuxiliaryAttempt],
    ) -> Result<()> {
        ensure!(
            self.turn_id.is_none(),
            "auxiliary settlement requires an idle journal"
        );
        if attempts.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock().expect("session journal poisoned");
        let existing = match &state.session {
            RunSession::Full(s) => s.active_entries(),
            RunSession::Window(w) => w.entries().iter().collect(),
        };
        let mut finished = std::collections::HashSet::new();
        for e in existing {
            if let SessionEntryData::Extension { namespace, data } = &e.data
                && namespace == crate::auxiliary::AUXILIARY_NAMESPACE
                && let crate::auxiliary::AuxiliaryRecord::Finished { attempt } =
                    serde_json::from_value(data.clone())?
            {
                finished.insert(attempt.id);
            }
        }
        let entries = attempts
            .iter()
            .filter(|a| !finished.contains(&a.id))
            .map(|a| {
                Ok(SessionEntryData::Extension {
                    namespace: crate::auxiliary::AUXILIARY_NAMESPACE.into(),
                    data: serde_json::to_value(crate::auxiliary::AuxiliaryRecord::Finished {
                        attempt: a.clone(),
                    })?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if entries.is_empty() {
            return Ok(());
        }
        self.write(&mut state, entries)
    }
    pub(crate) fn append_idle(&self, data: Vec<SessionEntryData>) -> Result<()> {
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(&mut state, data)
    }
    pub fn metadata(&self) -> SessionMetadata {
        self.state
            .lock()
            .expect("session journal poisoned")
            .session
            .metadata()
            .clone()
    }
    fn write(&self, state: &mut State, entries: Vec<SessionEntryData>) -> Result<()> {
        ensure!(!state.finished, "session journal already finished");
        if let Some(failure) = &state.failure {
            return Err(failure.clone().into());
        }
        if let Err(error) = state.session.append(self.repository, entries) {
            let failure = RunFailure::from_error(RunStage::Persistence, &error);
            state.failure = Some(failure.clone());
            return Err(failure.into());
        }
        Ok(())
    }
}
#[async_trait]
impl RunJournal for SessionJournal<'_> {
    async fn record_compaction_hooks(
        &self,
        record: &crate::summary::CompactionHookRecord,
    ) -> Result<()> {
        crate::extensions::size(record, 64 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::summary::COMPACTION_HOOK_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }

    async fn boundary_context(&self) -> Result<Option<super::OwnedSessionContext>> {
        let state = self.state.lock().expect("session journal poisoned");
        state
            .session
            .project_context(self.repository, self.turn_id.as_deref())
    }
    async fn prepare_boundary(
        &self,
        commit: &crate::extensions::BoundaryCommit,
        current: &[Message],
        prefix: usize,
    ) -> Result<crate::extensions::PreparedBoundary> {
        let state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.finished && state.failure.is_none(),
            "session journal is not writable"
        );
        let plan =
            state
                .session
                .prepare_append(self.repository, commit, self.turn_id.as_deref())?;
        let view = if commit.changes_projection() {
            let mut view = current
                .get(..prefix)
                .ok_or_else(|| anyhow::anyhow!("invalid boundary prefix"))?
                .to_vec();
            view.extend(plan.projection().messages.iter().cloned());
            view
        } else {
            commit.ephemeral_view(current, prefix)?
        };
        let mut prepared = crate::extensions::PreparedBoundary::new(commit.clone(), Some(view));
        prepared.set_session_projection(plan.projection().clone());
        prepared.bind(BoundaryPlan {
            owner: self.owner,
            plan,
        });
        Ok(prepared)
    }
    async fn commit_prepared_boundary(
        &self,
        prepared: crate::extensions::PreparedBoundary,
    ) -> Result<()> {
        let (commit, _, BoundaryPlan { owner, plan }) = prepared.into_parts::<BoundaryPlan>()?;
        ensure!(owner == self.owner, "foreign boundary journal plan");
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.finished && state.failure.is_none(),
            "session journal is not writable"
        );
        let ids = plan
            .transaction()
            .entries
            .iter()
            .filter(|e| e.data.as_message().is_some())
            .map(|e| e.id.clone())
            .collect::<Vec<_>>();
        let end = commit
            .base_index
            .checked_add(ids.len())
            .ok_or_else(|| anyhow::anyhow!("boundary index overflow"))?;
        ensure!(
            (commit.base_index..end).all(|i| !state.saved.contains_key(&i)),
            "boundary canonical positions already saved"
        );
        if let Err(error) = state.session.commit_append(self.repository, plan) {
            let failure = RunFailure::from_error(RunStage::Persistence, &error);
            state.failure = Some(failure.clone());
            return Err(failure.into());
        }
        for (i, id) in ids.into_iter().enumerate() {
            state.saved.insert(commit.base_index + i, id);
        }
        Ok(())
    }

    async fn compaction_projection(
        &self,
        view: &[Message],
        prefix: usize,
        input_messages: usize,
        history: &[Message],
    ) -> Result<Option<crate::summary::CompactionProjection>> {
        let state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.finished && state.failure.is_none(),
            "session journal cannot project compaction"
        );
        let Some(projection) = state
            .session
            .project_context(self.repository, self.turn_id.as_deref())?
        else {
            return Ok(None);
        };
        let offset = input_messages
            .checked_sub(state.initial_message_count)
            .ok_or_else(|| anyhow::anyhow!("invalid initial canonical positions"))?;
        let positions = state
            .initial_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), offset + i))
            .chain(state.saved.iter().map(|(i, id)| (id.as_str(), *i)));
        crate::summary::CompactionProjection::from_session_view(
            &projection,
            view,
            prefix,
            history,
            positions,
        )
    }

    async fn record_message_finalization(
        &self,
        record: &crate::extensions::MessageFinalizationRecord,
    ) -> Result<()> {
        crate::extensions::size(record, 64 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::extensions::MESSAGE_FINALIZATION_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn finalize_message(
        &self,
        commit: &crate::extensions::MessageFinalizationCommit,
    ) -> Result<()> {
        let entries = commit.session_entries()?;
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.saved.contains_key(&commit.index),
            "finalized message position already saved"
        );
        self.write(&mut state, entries)?;
        let id = state
            .session
            .metadata()
            .leaf_id
            .clone()
            .expect("finalized message checkpoint");
        state.saved.insert(commit.index, id);
        Ok(())
    }
    async fn record_finalization(
        &self,
        record: &crate::extensions::FinalizationRecord,
    ) -> Result<()> {
        crate::extensions::size(record, 64 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::extensions::FINALIZATION_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn finalize_assistant(
        &self,
        index: usize,
        record: &crate::extensions::FinalizationRecord,
        message: &Message,
    ) -> Result<()> {
        crate::extensions::size(record, 64 * 1024)?;
        crate::extensions::size(message, crate::extensions::MAX_FINAL_MESSAGE_BYTES)?;
        ensure!(
            message.role == MessageRole::Assistant,
            "finalized message has wrong role"
        );
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.saved.contains_key(&index),
            "finalized position already saved"
        );
        self.write(
            &mut state,
            vec![
                SessionEntryData::Extension {
                    namespace: crate::extensions::FINALIZATION_NAMESPACE.into(),
                    data: serde_json::to_value(record)?,
                },
                SessionEntryData::Message {
                    message: message.clone(),
                },
            ],
        )?;
        let id = state
            .session
            .metadata()
            .leaf_id
            .clone()
            .expect("finalized message");
        state.saved.insert(index, id);
        Ok(())
    }
    fn supports_custom_input(&self) -> bool {
        true
    }
    async fn input_custom(&self, index: usize, input: &crate::RunInput) -> Result<()> {
        let custom = input
            .custom_message()?
            .ok_or_else(|| anyhow::anyhow!("custom control metadata missing"))?;
        ensure!(
            input.provenance.is_none(),
            "custom control contains user provenance"
        );
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.saved.contains_key(&index),
            "custom control position already saved"
        );
        self.write(&mut state,vec![SessionEntryData::Extension{namespace:"aicoder.custom-delivery.v1".into(),data:serde_json::json!({"run_id":input.run_id,"id":input.id,"kind":input.kind,"source":custom.source(),"custom_type":custom.custom_type()})},SessionEntryData::CustomMessage{custom}])?;
        let id = state
            .session
            .metadata()
            .leaf_id
            .clone()
            .expect("custom checkpoint");
        state.saved.insert(index, id);
        Ok(())
    }
    async fn checkpoint(&self, index: usize, message: &Message) -> Result<()> {
        let mut state = self.state.lock().expect("session journal poisoned");
        if let Some(entry_id) = state.saved.get(&index) {
            ensure!(
                state
                    .session
                    .entry(entry_id)
                    .expect("saved checkpoint")
                    .data
                    .as_message()
                    == Some(message),
                "checkpoint position changed"
            );
            return Ok(());
        }
        self.write(
            &mut state,
            vec![SessionEntryData::Message {
                message: message.clone(),
            }],
        )?;
        let entry_id = state
            .session
            .metadata()
            .leaf_id
            .clone()
            .expect("checkpoint entry");
        state.saved.insert(index, entry_id);
        Ok(())
    }
    async fn input(&self, index: usize, input: &crate::RunInput) -> Result<()> {
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.saved.contains_key(&index),
            "control input position already saved"
        );
        self.write(
            &mut state,
            vec![
                SessionEntryData::Extension {
                    namespace: "aicoder.run.input".into(),
                    data: serde_json::json!({"run_id":input.run_id,"id":input.id,"kind":input.kind}),
                },
                SessionEntryData::Message {
                    message: input.message.clone(),
                },
            ],
        )?;
        let entry_id = state
            .session
            .metadata()
            .leaf_id
            .clone()
            .expect("control input checkpoint");
        state.saved.insert(index, entry_id);
        Ok(())
    }
    async fn record_input_context(&self, provenance: &serde_json::Value) -> Result<()> {
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: "aicoder.input_context".into(),
                data: provenance.clone(),
            }],
        )
    }
    async fn record_composition(
        &self,
        record: &crate::composition::CompositionRecord,
    ) -> Result<()> {
        crate::extensions::size(record, 512 * 1024)?;
        let data = serde_json::to_value(record)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::composition::COMPOSITION_NAMESPACE.into(),
                data,
            }],
        )
    }
    async fn record_input_hooks(&self, record: &crate::extensions::InputHookRecord) -> Result<()> {
        crate::extensions::size(record, crate::extensions::MAX_INPUT_HOOK_BYTES)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::extensions::INPUT_HOOK_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn input_prepared(
        &self,
        index: usize,
        input: &crate::RunInput,
        receipt: &crate::extensions::InputReceipt,
    ) -> Result<()> {
        let record = crate::extensions::InputHookRecord::Prepared {
            receipt: receipt.for_record(),
        };
        crate::extensions::size(&record, crate::extensions::MAX_INPUT_HOOK_BYTES)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.saved.contains_key(&index),
            "control input position already saved"
        );
        self.write(&mut state,vec![
            SessionEntryData::Extension{namespace:crate::extensions::INPUT_HOOK_NAMESPACE.into(),data:serde_json::to_value(record)?},
            SessionEntryData::Extension{namespace:"aicoder.run.input".into(),data:serde_json::json!({"run_id":input.run_id,"id":input.id,"kind":input.kind})},
            SessionEntryData::Message{message:input.message.clone()},
        ])?;
        let id = state
            .session
            .metadata()
            .leaf_id
            .clone()
            .expect("prepared input checkpoint");
        state.saved.insert(index, id);
        Ok(())
    }
    async fn record_lifecycle(&self, record: &crate::extensions::LifecycleRecord) -> Result<()> {
        crate::extensions::size(record, 128 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::extensions::LIFECYCLE_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn commit_boundary(&self, commit: &crate::extensions::BoundaryCommit) -> Result<()> {
        crate::extensions::size(commit, 128 * 1024)?;
        let messages = commit.messages();
        let mut state = self.state.lock().expect("session journal poisoned");
        let end = commit
            .base_index
            .checked_add(messages.len())
            .ok_or_else(|| anyhow::anyhow!("boundary index overflow"))?;
        ensure!(
            (commit.base_index..end).all(|i| !state.saved.contains_key(&i)),
            "boundary canonical positions already saved"
        );
        let entries = commit.session_entries()?;
        self.write(&mut state, entries)?;
        let ids = state
            .session
            .messages()
            .into_iter()
            .rev()
            .take(messages.len())
            .map(|m| m.id.to_string())
            .collect::<Vec<_>>();
        for (offset, id) in ids.into_iter().rev().enumerate() {
            state.saved.insert(commit.base_index + offset, id);
        }
        Ok(())
    }
    async fn record_request_view(
        &self,
        record: &crate::extensions::RequestViewRecord,
    ) -> Result<()> {
        crate::extensions::size(record, 64 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::extensions::REQUEST_VIEW_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn record_interception(
        &self,
        record: &crate::extensions::InterceptionRecord,
    ) -> Result<()> {
        crate::extensions::size(record, 96 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::extensions::INTERCEPTION_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn record_auxiliary(&self, record: &crate::auxiliary::AuxiliaryRecord) -> Result<()> {
        crate::extensions::size(record, 64 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: crate::auxiliary::AUXILIARY_NAMESPACE.into(),
                data: serde_json::to_value(record)?,
            }],
        )
    }
    async fn composition_state(
        &self,
        namespace: &str,
        max_bytes: usize,
    ) -> Result<Option<serde_json::Value>> {
        ensure!(
            !namespace.is_empty()
                && namespace.len() <= 128
                && (1024..=256 * 1024).contains(&max_bytes),
            "invalid composition-state bounds"
        );
        let state = self.state.lock().expect("session journal poisoned");
        let metadata = state.session.metadata();
        let saved = self
            .repository
            .extension_state(&metadata.id, namespace, max_bytes)?;
        if let Some(saved) = saved {
            ensure!(
                saved.metadata.revision == metadata.revision
                    && saved.metadata.leaf_id == metadata.leaf_id,
                "composition state source changed"
            );
            Ok(Some(saved.data))
        } else {
            Ok(None)
        }
    }
    async fn commit_composition_state(
        &self,
        namespace: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        ensure!(
            !namespace.is_empty() && namespace.len() <= 128,
            "invalid composition-state namespace"
        );
        crate::extensions::size(value, 256 * 1024)?;
        let mut state = self.state.lock().expect("session journal poisoned");
        self.write(
            &mut state,
            vec![SessionEntryData::Extension {
                namespace: namespace.into(),
                data: value.clone(),
            }],
        )
    }
    async fn record_summary(
        &self,
        draft: &crate::summary::SummaryDraft,
        input_messages: usize,
        history_messages: usize,
    ) -> Result<Option<String>> {
        let mut state = self.state.lock().expect("session journal poisoned");
        ensure!(
            !state.finished && state.failure.is_none(),
            "session journal cannot record a summary"
        );
        let offset = history_messages
            .checked_sub(draft.keep_messages)
            .ok_or_else(|| anyhow::anyhow!("invalid summary tail"))?;
        let first = if offset >= input_messages {
            state
                .saved
                .get(&offset)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("summary tail is not checkpointed"))?
        } else {
            let remaining = input_messages - offset;
            let position = state
                .initial_message_count
                .checked_sub(remaining)
                .ok_or_else(|| anyhow::anyhow!("summary would replace runtime instructions"))?;
            state
                .initial_ids
                .get(position)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("initial summary anchor missing"))?
        };
        let result = match &mut state.session {
            RunSession::Full(session) => crate::summary::session::commit_summary(
                self.repository,
                session,
                draft.clone(),
                first,
                self.turn_id.clone(),
            ),
            RunSession::Window(window) => {
                let checkpoint = crate::summary::SummaryCheckpoint {
                    version: 2,
                    source_leaf: window
                        .metadata()
                        .leaf_id
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("empty summary source"))?,
                    source_revision: window.metadata().revision,
                    first_kept_entry_id: first,
                    summarized_messages: window
                        .source()
                        .recorded_messages
                        .checked_sub(draft.keep_messages)
                        .ok_or_else(|| anyhow::anyhow!("invalid summary tail"))?,
                    accounted_by_turn: self.turn_id.clone(),
                    draft: draft.clone(),
                };
                self.repository
                    .append_window(window, vec![checkpoint.data()])
                    .map(|()| window.metadata().leaf_id.clone().expect("summary entry"))
            }
        };
        match result {
            Ok(id) => Ok(Some(id)),
            Err(error) => {
                let failure = RunFailure::from_error(RunStage::Persistence, &error);
                state.failure = Some(failure.clone());
                Err(failure.into())
            }
        }
    }

    async fn finish(&self, outcome: &RunOutcome) -> Result<()> {
        let mut state = self.state.lock().expect("session journal poisoned");
        let progress = outcome.progress();
        let mut entries = Vec::new();
        let mut calls = HashMap::new();
        let mut matched = 0;
        for (index, message) in progress
            .messages
            .iter()
            .enumerate()
            .skip(progress.input_messages)
        {
            let key = if message.role == MessageRole::Tool {
                let (id, _, _) = message
                    .tool_result()
                    .ok_or_else(|| anyhow::anyhow!("invalid generated tool result"))?;
                *calls
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("generated tool result has no call"))?
            } else {
                calls = message
                    .tool_calls()
                    .enumerate()
                    .map(|(offset, call)| (call.id.clone(), index + 1 + offset))
                    .collect();
                index
            };
            if let Some(entry_id) = state.saved.get(&key) {
                ensure!(
                    state
                        .session
                        .entry(entry_id)
                        .expect("saved checkpoint")
                        .data
                        .as_message()
                        == Some(message),
                    "checkpoint differs from final transcript"
                );
                matched += 1;
            } else {
                entries.push(SessionEntryData::Message {
                    message: message.clone(),
                });
            }
        }
        ensure!(
            matched == state.saved.len(),
            "final transcript omitted a checkpoint"
        );
        entries.push(SessionEntryData::TurnEnd {
            turn_id: self
                .turn_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("idle journal has no turn to finish"))?,
            record: Box::new(TurnRecord::from_progress(progress, outcome.stop().cloned())),
        });
        self.write(&mut state, entries)?;
        state.finished = true;
        Ok(())
    }
}
