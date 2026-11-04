use super::*;
use crate::extensions::*;
use std::time::Duration;
impl TurnExecutor {
    pub(crate) fn has_request_hooks(&self) -> bool {
        !self.request_hooks.is_empty()
    }
    pub(crate) async fn prepare_request_view(
        &self,
        mut request: ModelRequest,
        mut prefix: usize,
        history: &[Message],
        round: usize,
        pass: RequestHookPass,
        context: &TurnExecutionContext,
    ) -> Result<PreparedRequestView> {
        let mut changes = Vec::new();
        if !self.request_hooks.is_empty() {
            let spec = self.provider.model_spec(&request.model)?;
            let work = async {
                crate::extensions::size(&request, MAX_REQUEST_VIEW_BYTES)?;
                let mut bytes = 0usize;
                for registration in &self.request_hooks {
                    let hook = &registration.hook;
                    let edit = crate::extensions::invoke(
                        &hook.owner,
                        &hook.spec,
                        context.ui(),
                        |cancellation| {
                            hook.callback.request(
                                RequestHookInput {
                                    workspace: self.workspace_root(),
                                    round,
                                    pass,
                                    request: &request,
                                    history,
                                    system_prefix_len: prefix,
                                    spec: &spec,
                                },
                                cancellation,
                            )
                        },
                    )
                    .await?;
                    if let Some(edit) = edit {
                        let flags = crate::extensions::apply_edit(
                            &mut request,
                            &mut prefix,
                            edit,
                            registration.scope,
                        )?;
                        bytes = bytes.saturating_add(crate::extensions::size(
                            &request,
                            MAX_REQUEST_VIEW_BYTES,
                        )?);
                        anyhow::ensure!(
                            bytes <= 2 * MAX_REQUEST_VIEW_BYTES,
                            "request hook aggregate view budget exceeded"
                        );
                        changes.push(RequestChange {
                            extension: hook.owner.clone(),
                            hook: hook.spec.id.clone(),
                            scope: registration.scope,
                            messages_changed: flags.messages,
                            options_changed: flags.options,
                            tools_changed: flags.tools,
                        });
                    }
                    self.validate_request_view(&request, pass)?;
                }
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! {biased;
                cancelled=context.cancelled()=>return Err(cancelled.into()),
                result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("request hook phase timed out"))??,
            }
        }
        Ok(PreparedRequestView {
            owner: self.request_owner,
            round,
            request,
            prefix,
            changes,
            pass,
        })
    }
    pub(crate) fn validate_request_view(
        &self,
        request: &ModelRequest,
        pass: RequestHookPass,
    ) -> Result<()> {
        // Empty branch selection is a read-only preview, never an emitted synthetic user fact.
        if request.messages.is_empty() && pass == RequestHookPass::Preparation {
            let mut preview = request.clone();
            preview.messages.push(Message::text(
                crate::model::MessageRole::User,
                "[Empty-branch compatibility check]",
            ));
            self.provider.validate_request(&preview)?;
        } else {
            self.provider.validate_request(request)?;
        }
        Ok(())
    }
    pub(crate) async fn request_view_record(
        &self,
        view: &PreparedRequestView,
        stage: RequestViewStage,
        source_messages: usize,
        count: crate::tokens::TokenCount,
        journal: &dyn crate::RunJournal,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        if !self.has_request_hooks() {
            return Ok(());
        }
        let spec = self.provider.model_spec(&view.request.model)?;
        let record = RequestViewRecord {
            round: view.round,
            pass: view.pass,
            stage,
            provider: spec.provider,
            model: view.request.model.clone(),
            source_messages,
            messages: view.request.messages.len(),
            system_prefix_len: view.prefix,
            tools: view.request.tools.iter().map(|t| t.name.clone()).collect(),
            tool_choice: view.request.tool_choice.clone(),
            changes: view.changes.clone(),
            input_count: count,
            effective_options: RequestOptionSummary::from(&view.request.options),
        };
        crate::extensions::size(&record, 64 * 1024)?;
        tokio::select! {biased;
            cancelled=context.cancelled()=>Err(cancelled.into()),
            result=tokio::time::timeout(Duration::from_secs(30),journal.record_request_view(&record))=>result.map_err(|_|anyhow::anyhow!("request view journal timed out")).and_then(|v|v).map_err(|e|RunFailure::from_error(RunStage::Persistence,&e).into()),
        }
    }
}
