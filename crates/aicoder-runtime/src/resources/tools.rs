use super::*;
use aicoder_core::{
    model::ToolDefinition,
    tools::{ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess},
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
pub(super) struct LoadSkill(pub Arc<ResourceSnapshot>);
pub(super) struct ReadSkillFile(pub Arc<ResourceSnapshot>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillArguments {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileArguments {
    name: String,
    path: PathBuf,
    #[serde(default)]
    offset_bytes: u64,
    #[serde(default = "default_page")]
    limit_bytes: usize,
}
fn default_page() -> usize {
    16384
}
fn failure(error: anyhow::Error) -> ToolFailure {
    ToolFailure {
        code: "resource_failed".into(),
        message: format!("{error:#}"),
        details: None,
    }
}
#[async_trait]
impl ExecutableTool for LoadSkill {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {name:"load_skill".into(),description:Some("Load a relevant skill's frozen instructions and asset directory before using it. Does not grant approval or execute scripts.".into()),parameters:json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let args: SkillArguments = serde_json::from_value(args)
            .map_err(|_| failure(anyhow::anyhow!("Invalid skill arguments")))?;
        let text = self.0.load_skill(&args.name, false).map_err(failure)?;
        let output =
            json!({"name":args.name,"content":text,"generation":self.0.manifest.generation});
        if serde_json::to_vec(&output)
            .map_err(|_| failure(anyhow::anyhow!("Cannot encode skill")))?
            .len()
            > context.max_output_bytes()
        {
            return Err(failure(anyhow::anyhow!(
                "Skill exceeds tool output capacity; page SKILL.md with read_skill_file"
            )));
        }
        Ok(ToolSuccess {
            output,
            truncated: false,
        })
    }
}
#[async_trait]
impl ExecutableTool for ReadSkillFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {name:"read_skill_file".into(),description:Some("Read a bounded UTF-8 byte page from a known skill package. Paths stay within that package; follow returned offsets for more.".into()),parameters:json!({"type":"object","properties":{"name":{"type":"string"},"path":{"type":"string"},"offset_bytes":{"type":"integer","minimum":0},"limit_bytes":{"type":"integer","minimum":1,"maximum":32768}},"required":["name","path"],"additionalProperties":false})}
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        let args: FileArguments = serde_json::from_value(args)
            .map_err(|_| failure(anyhow::anyhow!("Invalid skill file arguments")))?;
        let limit = args
            .limit_bytes
            .min((context.max_output_bytes() / 8).max(4));
        let output = self
            .0
            .read_skill_file(&args.name, &args.path, args.offset_bytes, limit)
            .map_err(failure)?;
        if serde_json::to_vec(&output)
            .map_err(|_| failure(anyhow::anyhow!("Cannot encode resource page")))?
            .len()
            > context.max_output_bytes()
        {
            return Err(failure(anyhow::anyhow!(
                "Resource page metadata exceeds tool output capacity"
            )));
        }
        Ok(ToolSuccess {
            output,
            truncated: false,
        })
    }
}
