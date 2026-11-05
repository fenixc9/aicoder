//! Execution-derived observations, never instructions or restored host authority.
use aicoder_core::{
    RunJournal, RunOutcome, TurnExecutor, composition::CompositionRecord, model::*,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Mutex};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalProvenance {
    pub request_sha256: String,
    pub model_options: ModelOptions,
    pub requested_stream: bool,
    pub workspace_sha256: String,
    pub workspace: std::path::PathBuf,
    pub model: Option<ModelSpec>,
    pub execution: aicoder_core::TurnExecutionConfig,
    pub dispatcher: aicoder_core::tools::DispatcherConfig,
    pub tools: Vec<ToolDefinition>,
    /// Requested effective root declarations when narrower than the installed leaf registry.
    /// Observation only; replay never restores tool authority from this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_tools: Option<Vec<ToolDefinition>>,
    pub tool_capabilities: BTreeMap<String, aicoder_core::tools::ToolCapability>,
    pub extensions: Value,
    pub context_policy: String,
    pub journal: Vec<Value>,
}
impl EvalProvenance {
    pub(crate) fn capture(
        executor: &TurnExecutor,
        request: &ModelRequest,
        workspace_sha256: String,
    ) -> Result<Self> {
        let snapshot = Self {
            request_sha256: crate::bounds::sha256(&crate::bounds::encode(
                request,
                4 * 1024 * 1024,
            )?),
            model_options: request.options.clone(),
            requested_stream: request.stream,
            workspace_sha256,
            workspace: executor.workspace_root().into(),
            model: executor
                .model_spec(&request.model)
                .ok()
                .filter(|spec| spec.validate().is_ok()),
            execution: executor.execution_config().clone(),
            dispatcher: executor.dispatcher_config().clone(),
            tools: executor.tool_registry().definitions(),
            model_tools: {
                let offered = if request.tools.is_empty() {
                    executor.model_tool_definitions()
                } else {
                    request.tools.clone()
                };
                (offered != executor.tool_registry().definitions()).then_some(offered)
            },
            tool_capabilities: executor
                .tool_registry()
                .definitions()
                .into_iter()
                .map(|tool| {
                    let capability = executor
                        .tool_registry()
                        .get(&tool.name)
                        .expect("frozen registry tool")
                        .capability();
                    (tool.name, capability)
                })
                .collect(),
            extensions: serde_json::to_value(executor.extension_catalog())?,
            context_policy: executor.context_policy_name().into(),
            journal: Vec::new(),
        };
        let _: Self = serde_json::from_slice(&crate::bounds::encode(&snapshot, 4 * 1024 * 1024)?)?;
        Ok(snapshot)
    }
}
#[derive(Default)]
struct State {
    values: BTreeMap<String, Value>,
    records: Vec<Value>,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct EvaluationJournal(Mutex<State>);
impl EvaluationJournal {
    fn record(&self, value: Value) -> Result<()> {
        let mut state = self.0.lock().expect("evaluation journal poisoned");
        Self::append(&mut state, value)
    }
    fn append(state: &mut State, value: Value) -> Result<()> {
        ensure!(
            state.records.len() < 4096,
            "evaluation journal record budget exhausted"
        );
        let size = crate::bounds::encoded_size(
            &value,
            (8 * 1024 * 1024_usize).saturating_sub(state.bytes),
        )?;
        state.bytes += size;
        state.records.push(value);
        Ok(())
    }
    pub fn records(&self) -> Vec<Value> {
        self.0
            .lock()
            .expect("evaluation journal poisoned")
            .records
            .clone()
    }
}
#[async_trait]
impl RunJournal for EvaluationJournal {
    async fn record_input_hooks(
        &self,
        record: &aicoder_core::extensions::InputHookRecord,
    ) -> Result<()> {
        self.record(json!({"kind":"input_hooks","data":record}))
    }
    async fn input_prepared(
        &self,
        index: usize,
        input: &aicoder_core::RunInput,
        receipt: &aicoder_core::extensions::InputReceipt,
    ) -> Result<()> {
        self.record(json!({"kind":"prepared_input","index":index,"input":input,"receipt":receipt}))
    }
    async fn record_lifecycle(
        &self,
        record: &aicoder_core::extensions::LifecycleRecord,
    ) -> Result<()> {
        self.record(json!({"kind":"lifecycle","data":record}))
    }
    async fn commit_boundary(
        &self,
        commit: &aicoder_core::extensions::BoundaryCommit,
    ) -> Result<()> {
        self.record(json!({"kind":"lifecycle_boundary","data":commit}))
    }
    async fn record_request_view(
        &self,
        record: &aicoder_core::extensions::RequestViewRecord,
    ) -> Result<()> {
        self.record(json!({"kind":"request_view","data":record}))
    }
    async fn record_interception(
        &self,
        record: &aicoder_core::extensions::InterceptionRecord,
    ) -> Result<()> {
        self.record(json!({"kind":"interception","data":record}))
    }
    async fn record_auxiliary(
        &self,
        record: &aicoder_core::auxiliary::AuxiliaryRecord,
    ) -> Result<()> {
        self.record(json!({"kind":"auxiliary","data":record}))
    }

    async fn checkpoint(&self, _: usize, _: &Message) -> Result<()> {
        Ok(())
    }
    async fn record_input_context(&self, value: &Value) -> Result<()> {
        crate::bounds::encoded_size(value, 1024 * 1024)?;
        self.record(json!({"kind":"input_context","data":value}))
    }
    async fn record_composition(&self, record: &CompositionRecord) -> Result<()> {
        crate::bounds::encoded_size(record, 512 * 1024)?;
        self.record(json!({"kind":"composition","data":record}))
    }
    async fn composition_state(&self, namespace: &str, max_bytes: usize) -> Result<Option<Value>> {
        let state = self.0.lock().expect("evaluation journal poisoned");
        match state.values.get(namespace) {
            Some(value) => {
                crate::bounds::encoded_size(value, max_bytes)?;
                Ok(Some(value.clone()))
            }
            None => Ok(None),
        }
    }
    async fn commit_composition_state(&self, namespace: &str, value: &Value) -> Result<()> {
        ensure!(
            !namespace.is_empty() && namespace.len() <= 128,
            "invalid public state namespace"
        );
        crate::bounds::encoded_size(value, 256 * 1024)?;
        let mut state = self.0.lock().expect("evaluation journal poisoned");
        ensure!(
            state.values.contains_key(namespace) || state.values.len() < 16,
            "evaluation public state namespace budget"
        );
        Self::append(
            &mut state,
            json!({"kind":"public_state","namespace":namespace,"data":value}),
        )?;
        state.values.insert(namespace.into(), value.clone());
        Ok(())
    }
    async fn finish(&self, _: &RunOutcome) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tool_catalog_tests {
    use super::*;
    use aicoder_core::{ModelProvider, TurnExecutionContext, tools::*};
    use std::sync::Arc;
    struct Provider;
    #[async_trait]
    impl ModelProvider for Provider {
        fn model_spec(&self, model: &str) -> Result<ModelSpec, ProviderError> {
            let mut spec = ModelSpec::new("fixture", model);
            spec.capabilities.tools = true;
            Ok(spec)
        }
        async fn generate(
            &self,
            _: ModelRequest,
            _: Arc<dyn ModelEventSink>,
            _: TurnExecutionContext,
        ) -> Result<ModelResponse, ProviderError> {
            panic!("capture does not invoke models")
        }
    }
    struct Tool(&'static str);
    #[async_trait]
    impl ExecutableTool for Tool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: None,
                parameters: json!({"type":"object"}),
            }
        }
        fn capability(&self) -> ToolCapability {
            ToolCapability::ReadOnly
        }
        async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolSuccess, ToolFailure> {
            panic!("capture does not execute tools")
        }
    }
    #[test]
    fn provenance_records_root_exposure_and_accepts_old_observations_without_restoring_authority() {
        let root = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(Tool("outer")).unwrap();
        registry.register(Tool("leaf")).unwrap();
        let executor = TurnExecutor::builder(Provider)
            .workspace(root.path())
            .registry(registry)
            .model_tools(vec!["outer".into()])
            .build()
            .unwrap();
        let request = ModelRequest::new("fixture", vec![Message::text(MessageRole::User, "input")]);
        let observed = EvalProvenance::capture(&executor, &request, "workspace".into()).unwrap();
        assert_eq!(observed.tools.len(), 2);
        assert_eq!(observed.model_tools.as_ref().unwrap()[0].name, "outer");
        let mut old = serde_json::to_value(observed).unwrap();
        old.as_object_mut().unwrap().remove("model_tools");
        assert!(
            serde_json::from_value::<EvalProvenance>(old)
                .unwrap()
                .model_tools
                .is_none()
        );
    }
}
