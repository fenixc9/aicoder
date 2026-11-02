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
