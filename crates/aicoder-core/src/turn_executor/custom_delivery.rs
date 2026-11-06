use super::*;
pub(super) struct CustomScope<'a> {
    pub request: &'a ModelRequest,
    pub messages: &'a mut Vec<Message>,
    pub progress: &'a mut RunProgress,
    pub context: &'a TurnExecutionContext,
    pub limits: &'a Arc<RunLimiter>,
    pub journal: &'a dyn crate::RunJournal,
    pub events: &'a AgentEventEmitter,
}
impl TurnExecutor {
    pub(super) async fn flush_custom_context(
        &self,
        scope: CustomScope<'_>,
        candidate: Option<(usize, &Message)>,
    ) -> Result<()> {
        let CustomScope {
            request,
            messages,
            progress,
            context,
            limits,
            journal,
            events,
        } = scope;
        let Some(control) = &context.control else {
            return Ok(());
        };
        if !control.has_context() {
            return Ok(());
        }
        if let Some((index, message)) = candidate {
            checkpoint(journal, index, message, context).await?;
        }
        let work = async {
            for _ in 0..256 {
                limits.check()?;
                let Some(input) = control.next_context() else {
                    return Ok(());
                };
                commit_run_input(
                    input,
                    messages,
                    progress,
                    context,
                    (journal, limits),
                    events,
                    &|messages, input| self.validate_control_input(request, messages, input),
                )
                .await?;
            }
            anyhow::ensure!(
                !control.has_context(),
                "deferred custom input phase exceeds admission budget"
            );
            Ok(())
        };
        tokio::select! {biased;cancelled=context.cancelled()=>Err(cancelled.into()),result=tokio::time::timeout(std::time::Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("deferred custom input phase timed out"))?}
    }
}
