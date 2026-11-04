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
    turn_id: String,
    state: Mutex<State>,
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
        let initial_ids = session
            .messages()
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>();
        let initial_message_count = initial_ids.len();
        Self {
            repository,
            turn_id,
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
    async fn checkpoint(&self, index: usize, message: &Message) -> Result<()> {
        let mut state = self.state.lock().expect("session journal poisoned");
        if let Some(entry_id) = state.saved.get(&index) {
            ensure!(
                matches!(&state.session.entry(entry_id).expect("saved checkpoint").data,SessionEntryData::Message {message:saved} if saved==message),
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
                Some(self.turn_id.clone()),
            ),
            RunSession::Window(window) => {
                let checkpoint = crate::summary::SummaryCheckpoint {
                    version: 1,
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
                    accounted_by_turn: Some(self.turn_id.clone()),
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
                    matches!(&state.session.entry(entry_id).expect("saved checkpoint").data,SessionEntryData::Message {message:saved} if saved==message),
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
            turn_id: self.turn_id.clone(),
            record: Box::new(TurnRecord::from_progress(progress, outcome.stop().cloned())),
        });
        self.write(&mut state, entries)?;
        state.finished = true;
        Ok(())
    }
}
