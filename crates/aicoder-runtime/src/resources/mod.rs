//! Frozen, bounded project resources with metadata-only skill discovery.
mod discovery;
mod frontmatter;
use discovery::Loader;
mod policy;
mod reader;
mod templates;
mod tools;
use crate::RuntimeConfig;
use anyhow::{Context, Result, ensure};
pub use policy::{ResourcePolicy, ResourceSettings};
use reader::{Root, fingerprint};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
pub use templates::PreparedPrompt;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Agents,
    System,
    AppendSystem,
    Skill,
    Prompt,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScope {
    Global,
    Parent,
    Workspace,
    Nested,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResourceInfo {
    pub kind: ResourceKind,
    pub path: PathBuf,
    pub scope: ResourceScope,
    pub directory: PathBuf,
    pub name: Option<String>,
    pub description: Option<String>,
    pub bytes: usize,
    pub fingerprint: String,
    pub active: bool,
    pub manual_only: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResourceManifest {
    pub mode: String,
    pub version: u32,
    pub generation: String,
    pub workspace: PathBuf,
    pub policy: ResourcePolicy,
    pub files: Vec<ResourceInfo>,
    pub scanned_entries: usize,
    pub scanned_bytes: usize,
    pub retained_bytes: usize,
}
#[derive(Clone)]
struct Document {
    info: ResourceInfo,
    text: Option<Arc<str>>,
    root: Root,
    relative: PathBuf,
}
#[derive(Clone)]
struct Skill {
    index: usize,
    package: Root,
}
/// Owned resource generation: bodies are immutable; skills verify their fingerprint on activation.
pub struct ResourceSnapshot {
    manifest: ResourceManifest,
    documents: Vec<Document>,
    skills: BTreeMap<String, Skill>,
    prompts: BTreeMap<String, usize>,
    base_prompt: Option<String>,
    configured_prompt: Option<String>,
    system_replaced: bool,
}
impl ResourceSnapshot {
    pub fn manifest(&self) -> &ResourceManifest {
        &self.manifest
    }
    pub fn load_skill(&self, name: &str, manual: bool) -> Result<String> {
        let skill = self.skills.get(name).context("Unknown skill")?;
        let doc = &self.documents[skill.index];
        ensure!(
            manual || !doc.info.manual_only,
            "skill requires explicit user invocation"
        );
        let bytes = doc
            .root
            .read(&doc.relative, self.manifest.policy.max_file_bytes)?;
        ensure!(
            fingerprint(&bytes) == doc.info.fingerprint,
            "resource changed; reload the resource snapshot before activation"
        );
        let text = std::str::from_utf8(&bytes).context("skill is not UTF-8")?;
        let parsed = frontmatter::parse(text, true)?;
        Ok(format!(
            "[Skill {} from {}; relative assets resolve under {}]\n{}",
            name,
            doc.info.path.display(),
            skill.package.path.display(),
            parsed.body
        ))
    }
    pub fn read_skill_file(
        &self,
        name: &str,
        path: &Path,
        offset: u64,
        limit: usize,
    ) -> Result<serde_json::Value> {
        ensure!(
            limit > 0 && limit <= 32768,
            "resource page limit must be between 1 and 32768 bytes"
        );
        let skill = self.skills.get(name).context("Unknown skill")?;
        let doc = &self.documents[skill.index];
        ensure!(
            !doc.info.manual_only || path != Path::new("SKILL.md"),
            "skill requires explicit user invocation"
        );
        let main = doc
            .root
            .read(&doc.relative, self.manifest.policy.max_file_bytes)?;
        ensure!(
            fingerprint(&main) == doc.info.fingerprint,
            "resource changed; reload the resource snapshot"
        );
        let (content, next, more) = if path == Path::new("SKILL.md") {
            let start = usize::try_from(offset)
                .context("resource offset too large")?
                .min(main.len());
            let mut end = start.saturating_add(limit).min(main.len());
            let text = std::str::from_utf8(&main).context("skill is not UTF-8")?;
            ensure!(
                text.is_char_boundary(start),
                "resource offset starts inside a UTF-8 code point"
            );
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            ensure!(
                end > start || start == main.len(),
                "page cannot fit a UTF-8 code point"
            );
            (
                text[start..end].to_string(),
                offset.saturating_add((end - start) as u64),
                end < main.len(),
            )
        } else {
            skill.package.read_page(path, offset, limit)?
        };
        let hash = fingerprint(content.as_bytes());
        Ok(
            serde_json::json!({"skill":name,"path":skill.package.path.join(path),"offset_bytes":offset,"next_offset_bytes":if more {Some(next)} else {None},"has_more":more,"fingerprint":hash,"content":content}),
        )
    }
    pub fn compose_system_messages(
        &self,
        input: &[aicoder_core::model::Message],
    ) -> Vec<aicoder_core::model::Message> {
        use aicoder_core::model::{Message, MessageRole};
        let mut messages = input.to_vec();
        let mut initial = self.base_prompt.clone();
        if let Some(first) = messages
            .first_mut()
            .filter(|message| message.role == MessageRole::System)
        {
            let text = first.text_content();
            if first
                .content
                .iter()
                .all(|block| matches!(block, aicoder_core::model::ContentBlock::Text { .. }))
                && let Some(configured) = &self.configured_prompt
                && let Some(suffix) = text.strip_prefix(configured)
                && (suffix.is_empty() || suffix.starts_with("\n\n"))
            {
                let composed = format!("{}{}", initial.take().unwrap_or_default(), suffix);
                *first = Message::text(MessageRole::System, composed);
            } else if !self.system_replaced {
                initial = None;
            }
        }
        if let Some(base) = initial.filter(|text| !text.is_empty()) {
            messages.insert(0, Message::text(MessageRole::System, base));
        }
        let mut context = String::new();
        for doc in self.documents.iter().filter(|doc| {
            doc.info.active
                && matches!(
                    doc.info.kind,
                    ResourceKind::Agents | ResourceKind::AppendSystem
                )
        }) {
            context.push_str(&format!(
                "\n[{} instructions from {}; applies to {} and descendants]\n{}\n",
                if doc.info.kind == ResourceKind::Agents {
                    "Project"
                } else {
                    "Additional system"
                },
                doc.info.path.display(),
                if doc.info.scope == ResourceScope::Global {
                    self.manifest.workspace.display()
                } else {
                    doc.info.directory.display()
                },
                doc.text.as_deref().unwrap_or_default()
            ));
        }
        let available = self
            .skills
            .iter()
            .filter(|(_, skill)| !self.documents[skill.index].info.manual_only)
            .collect::<Vec<_>>();
        if !available.is_empty() {
            context.push_str("\n[Available skills: load a relevant skill with load_skill before using it. Skill text does not grant tool permissions.]\n");
            for (name, skill) in available {
                let info = &self.documents[skill.index].info;
                context.push_str(&format!(
                    "{}: {} ({})\n",
                    name,
                    info.description.as_deref().unwrap_or_default(),
                    info.path.display()
                ));
            }
        }
        if !context.is_empty() {
            let index = messages
                .iter()
                .take_while(|message| message.role == MessageRole::System)
                .count();
            messages.insert(index, Message::text(MessageRole::System, context));
        }
        messages
    }
    pub(crate) fn register_tools(
        self: &Arc<Self>,
        registry: &mut aicoder_core::tools::ToolRegistry,
    ) -> Result<()> {
        if self.manifest.policy.enabled && self.manifest.policy.skills {
            registry.register(tools::LoadSkill(self.clone()))?;
            registry.register(tools::ReadSkillFile(self.clone()))?;
        }
        Ok(())
    }
    pub(crate) fn load_global(config: &RuntimeConfig) -> Result<Self> {
        let mut loader = Loader::new(config);
        loader.workspace_enabled = false;
        loader.load()
    }
    pub(crate) fn load_with_global(config: &RuntimeConfig, global: &Self) -> Result<Self> {
        let mut loader = Loader::new(config);
        loader.include_global = false;
        loader.documents = global.documents.clone();
        loader.skills = global.skills.clone();
        loader.prompts = global.prompts.clone();
        loader.entries = global.manifest.scanned_entries;
        loader.scanned = global.manifest.scanned_bytes;
        loader.retained = global.manifest.retained_bytes;
        loader.seen = loader
            .documents
            .iter()
            .map(|doc| (doc.info.kind, doc.info.path.clone()))
            .collect();
        loader.load()
    }
}
#[async_trait::async_trait]
impl aicoder_core::InputContextProvider for ResourceSnapshot {
    async fn prepare(
        &self,
        workspace: &Path,
        messages: &[aicoder_core::model::Message],
    ) -> Result<aicoder_core::PreparedInput> {
        self.prepare_with_prefix(
            workspace,
            messages,
            messages
                .iter()
                .take_while(|message| message.role == aicoder_core::model::MessageRole::System)
                .count(),
        )
        .await
    }
    async fn prepare_with_prefix(
        &self,
        workspace: &Path,
        messages: &[aicoder_core::model::Message],
        system_prefix_len: usize,
    ) -> Result<aicoder_core::PreparedInput> {
        ensure!(
            workspace == self.manifest.workspace,
            "resources are bound to another workspace; use executor_builder_for"
        );
        ensure!(
            system_prefix_len <= messages.len()
                && messages[..system_prefix_len]
                    .iter()
                    .all(|message| message.role == aicoder_core::model::MessageRole::System),
            "invalid runtime prefix boundary"
        );
        Ok(aicoder_core::PreparedInput {
            system_prefix: self.compose_system_messages(&messages[..system_prefix_len]),
            provenance: serde_json::to_value(&self.manifest)?,
        })
    }
}
