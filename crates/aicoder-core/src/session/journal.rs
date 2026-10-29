use super::{Session, SessionEntryData, SessionMetadata, SessionRepository, TurnRecord};
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
    session: Session,
    saved: HashMap<usize, String>,
    failure: Option<RunFailure>,
    finished: bool,
}
impl<'a> SessionJournal<'a> {
    pub fn new(repository: &'a dyn SessionRepository, session: Session, turn_id: String) -> Self {
        Self {
            repository,
            turn_id,
            state: Mutex::new(State {
                session,
                saved: HashMap::new(),
                failure: None,
                finished: false,
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
        if let Err(error) = self.repository.append_entries(&mut state.session, entries) {
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
