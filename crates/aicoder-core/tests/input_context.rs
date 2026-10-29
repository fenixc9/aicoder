use aicoder_core::{model::*, tools::ToolRegistry, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Model(Arc<AtomicUsize>);
#[async_trait]
impl ModelProvider for Model {
    fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
        Ok(ModelSpec::new("offline", model))
    }
    async fn generate(
        &self,
        request: ModelRequest,
        sink: Arc<dyn ModelEventSink>,
        _: TurnExecutionContext,
    ) -> Result<ModelResponse, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let response = ModelResponse {
            id: None,
            model: request.model,
            message: Message::text(MessageRole::Assistant, "done"),
            stop_reason: StopReason::Complete,
            usage: ModelUsage::zero(),
        };
        emit_response(&response, sink.as_ref());
        Ok(response)
    }
}
struct Context {
    rewrite: bool,
    pending: bool,
}
#[async_trait]
impl InputContextProvider for Context {
    async fn prepare(&self, _: &Path, messages: &[Message]) -> anyhow::Result<PreparedInput> {
        if self.pending {
            return std::future::pending().await;
        }
        let mut system_prefix = messages
            .iter()
            .take_while(|message| message.role == MessageRole::System)
            .cloned()
            .collect::<Vec<_>>();
        if self.rewrite {
            system_prefix.push(Message::text(MessageRole::User, "rewritten history"));
        } else {
            system_prefix.insert(0, Message::text(MessageRole::System, "resource context"));
        }
        Ok(PreparedInput {
            system_prefix,
            provenance: json!({"version":1,"source":"fixture"}),
        })
    }
}
fn request() -> ModelRequest {
    ModelRequest::new(
        "offline",
        vec![
            Message::text(MessageRole::System, "base"),
            Message::text(MessageRole::User, "original"),
            Message::text(MessageRole::System, "trusted recovery marker"),
            Message::text(MessageRole::User, "continue"),
        ],
    )
}
#[tokio::test]
async fn context_can_change_initial_system_messages_but_cannot_rewrite_recorded_history() {
    for rewrite in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = TurnExecutor::builder(Model(calls.clone()))
            .workspace(std::env::temp_dir())
            .registry(ToolRegistry::default())
            .input_context(Arc::new(Context {
                rewrite,
                pending: false,
            }))
            .build()
            .unwrap();
        let outcome = executor
            .run_outcome(request(), Arc::new(()), TurnExecutionContext::new())
            .await;
        if rewrite {
            assert!(matches!(outcome, RunOutcome::Interrupted(_)));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        } else {
            assert!(matches!(outcome, RunOutcome::Completed(_)));
            assert_eq!(
                outcome.progress().messages[3].text_content(),
                "trusted recovery marker"
            );
            assert_eq!(outcome.progress().input_messages, 5);
            assert!(outcome.progress().input_context.is_some());
        }
    }
}
#[tokio::test(start_paused = true)]
async fn input_context_preparation_obeys_cancellation_and_the_execution_deadline() {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = TurnExecutor::builder(Model(calls.clone()))
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .input_context(Arc::new(Context {
            rewrite: false,
            pending: true,
        }))
        .config(TurnExecutionConfig {
            budget: RunBudget {
                max_duration: Some(Duration::from_millis(10)),
                ..Default::default()
            },
            ..Default::default()
        })
        .build()
        .unwrap();
    let outcome = executor
        .run_outcome(request(), Arc::new(()), TurnExecutionContext::new())
        .await;
    assert!(
        matches!(&outcome,RunOutcome::Interrupted(turn) if matches!(turn.stop,RunStop::Budget(_)))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(outcome.progress().rounds, 0);
}
struct Journal;
#[async_trait]
impl RunJournal for Journal {
    async fn checkpoint(&self, _: usize, _: &Message) -> anyhow::Result<()> {
        Ok(())
    }
    async fn record_input_context(&self, _: &serde_json::Value) -> anyhow::Result<()> {
        anyhow::bail!("context journal unavailable")
    }
    async fn finish(&self, _: &RunOutcome) -> anyhow::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn context_provenance_failure_stops_before_the_model_and_retains_prepared_progress() {
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = TurnExecutor::builder(Model(calls.clone()))
        .workspace(std::env::temp_dir())
        .registry(ToolRegistry::default())
        .input_context(Arc::new(Context {
            rewrite: false,
            pending: false,
        }))
        .build()
        .unwrap();
    let outcome = executor
        .run_with_journal(
            request(),
            Arc::new(()),
            TurnExecutionContext::new(),
            &Journal,
        )
        .await;
    assert!(
        matches!(&outcome,RunOutcome::Interrupted(turn) if matches!(&turn.stop,RunStop::Failed(failure) if failure.stage==RunStage::Persistence))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(outcome.progress().input_context.is_some());
}
