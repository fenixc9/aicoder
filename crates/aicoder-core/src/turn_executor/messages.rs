use super::*;
use crate::extensions::*;
impl TurnExecutor {
    pub(crate) fn check_message_preparation_duration(
        &self,
        started: tokio::time::Instant,
    ) -> Result<()> {
        if self
            .config
            .budget
            .max_duration
            .is_some_and(|limit| started.elapsed() >= limit)
        {
            return Err(self.preparation_budget_error(started).into());
        }
        Ok(())
    }
    pub(crate) async fn prepare_prompt_finalization(
        &self,
        prompt: &mut crate::AgentPrompt,
        context: &TurnExecutionContext,
        started: tokio::time::Instant,
    ) -> Result<Option<FinalizedMessage>> {
        self.check_message_preparation_duration(started)?;
        let metadata = match prompt {
            crate::AgentPrompt::Custom(c) => Some(c.metadata()),
            _ => None,
        };
        let result = self
            .prepare_message_finalization(
                prompt.message(),
                metadata.as_ref(),
                MessageOrigin::Initial,
                None,
                context,
            )
            .await?;
        self.check_message_preparation_duration(started)?;
        if let Some(finalized) = &result {
            prompt.replace_finalized_message(finalized.message.clone())?;
        }
        Ok(result)
    }
    pub(crate) async fn prepare_message_finalization(
        &self,
        message: &Message,
        custom: Option<&crate::CustomMessageMetadata>,
        origin: MessageOrigin,
        round: Option<usize>,
        context: &TurnExecutionContext,
    ) -> Result<Option<FinalizedMessage>> {
        match &self.message_finalizers {
            Some(chain) => chain.prepare(message, custom, origin, round, context).await,
            None => Ok(None),
        }
    }
    pub(crate) async fn finalize_context_commit(
        &self,
        commit: &mut BoundaryCommit,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        if self.message_finalizers.is_none() {
            return Ok(());
        }
        let origin = if commit.kind == BoundaryKind::BeforeStart {
            MessageOrigin::Startup
        } else {
            MessageOrigin::Boundary
        };
        for proposal in &mut commit.entries {
            let custom = proposal.custom_message()?;
            let original = if let Some(custom) = &custom {
                custom.message().clone()
            } else if let BoundaryEntry::Context { kind, text } = &proposal.entry {
                crate::session::NativeContextSource {
                    extension: proposal.extension.clone(),
                    hook: proposal.hook.clone(),
                    kind: kind.clone(),
                }
                .message(text)
            } else {
                continue;
            };
            let metadata = custom.as_ref().map(crate::CustomMessage::metadata);
            if let Some(value) = self
                .prepare_message_finalization(
                    &original,
                    metadata.as_ref(),
                    origin,
                    Some(commit.round),
                    context,
                )
                .await?
            {
                match &mut proposal.entry {
                    BoundaryEntry::Context { text, .. } => {
                        *text = value
                            .message
                            .text_content()
                            .split_once("]\n")
                            .expect("validated source label")
                            .1
                            .into()
                    }
                    BoundaryEntry::CustomMessage {
                        content,
                        display,
                        details,
                        ..
                    } => {
                        let custom = crate::CustomMessage::from_canonical(
                            value.custom.clone().expect("custom finalization metadata"),
                            value.message.clone(),
                        )?;
                        *content = custom.message().content[1..].to_vec();
                        *display = custom.display();
                        *details = custom.details().cloned();
                    }
                    _ => unreachable!(),
                }
                proposal.entry.validate()?;
                commit.message_finalizations.push(value.receipt);
            }
        }
        Ok(())
    }
    pub(crate) fn context_finalization_events(commit: &BoundaryCommit, events: &AgentEventEmitter) {
        let metadata = commit.entries.iter().filter_map(|p| match &p.entry {
            BoundaryEntry::Context { .. } => Some(None),
            BoundaryEntry::CustomMessage { .. } => Some(
                p.custom_message()
                    .expect("validated custom")
                    .map(|c| c.metadata()),
            ),
            _ => None,
        });
        for ((receipt, message), custom) in commit
            .message_finalizations
            .iter()
            .zip(commit.messages())
            .zip(metadata)
        {
            events.emit(
                FinalizedMessage {
                    receipt: receipt.clone(),
                    message,
                    custom,
                }
                .event(),
            );
        }
    }
}
