//! Frozen resource expansion after core raw-input hooks.
use aicoder_core::{AgentInput, extensions::InputExpander, model::ContentBlock};
use async_trait::async_trait;
#[async_trait]
impl InputExpander for crate::resources::ResourceSnapshot {
    async fn expand(&self, input: AgentInput) -> anyhow::Result<AgentInput> {
        let Some(ContentBlock::Text { text }) = input.message().content.first() else {
            return Ok(input);
        };
        let expanded = self.prepare_prompt(text)?.text;
        Ok(input.with_prompt_text(expanded))
    }
}
