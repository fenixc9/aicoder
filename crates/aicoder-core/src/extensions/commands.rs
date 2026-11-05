use super::ExtensionCancellation;
use crate::{AgentInput, session::SessionMetadata};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
pub struct CommandContext<'a> {
    pub workspace: &'a Path,
    pub session: Option<&'a SessionMetadata>,
}
pub struct CommandRequest<'a> {
    pub context: CommandContext<'a>,
    pub arguments: &'a Value,
}
/// The host displays text or submits the typed user/custom prompt through the same Agent API.
pub enum CommandOutput {
    Notice(String),
    Submit(AgentInput),
    SubmitCustom(crate::CustomMessage),
}
impl CommandOutput {
    pub fn into_prompt(self) -> Option<crate::AgentPrompt> {
        match self {
            Self::Notice(_) => None,
            Self::Submit(input) => Some(input.into()),
            Self::SubmitCustom(custom) => Some(custom.into()),
        }
    }
}
#[async_trait]
pub trait ExtensionCommand: Send + Sync {
    fn specification(&self) -> CommandSpec;
    async fn execute(
        &self,
        request: CommandRequest<'_>,
        cancellation: ExtensionCancellation,
    ) -> Result<CommandOutput>;
}
