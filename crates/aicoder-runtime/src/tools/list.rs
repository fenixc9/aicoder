use super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess, WorkspacePaths, io::*,
    tree, util::parse_arguments,
};
use aicoder_core::model::ToolDefinition;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

pub struct ListDirectoryTool;
pub struct GlobTool;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    #[serde(default = "default_path")]
    path: String,
    pattern: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    hidden: bool,
    #[serde(default = "yes")]
    respect_gitignore: bool,
    cursor: Option<String>,
}
fn default_path() -> String {
    ".".into()
}
fn default_limit() -> usize {
    100
}
fn yes() -> bool {
    true
}
fn definition(recursive: bool) -> ToolDefinition {
    let mut properties = json!({"path":{"type":"string","default":"."},"limit":{"type":"integer","minimum":1,"maximum":1000,"default":100},
        "hidden":{"type":"boolean","default":false},"respect_gitignore":{"type":"boolean","default":true},"cursor":{"type":"string"}});
    if recursive {
        properties["pattern"] = json!({"type":"string","description":"Glob relative to path; a basename pattern matches any depth"});
    }
    ToolDefinition { name: if recursive { "glob" } else { "list_directory" }.into(),
        description: Some(if recursive { "Find files by glob in stable path order, honoring project .gitignore. Pass next_cursor unchanged for more results." } else { "List immediate directory entries in stable path order, including symlink types without following them. Pass next_cursor for more results." }.into()),
        parameters: json!({"type":"object","properties":properties,"required":if recursive {vec!["pattern"]}else{vec![]},"additionalProperties":false}) }
}
#[async_trait]
impl ExecutableTool for ListDirectoryTool {
    fn definition(&self) -> ToolDefinition {
        definition(false)
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        if args.get("pattern").is_some() {
            return Err(failure(
                "invalid_arguments",
                "list_directory does not accept pattern",
            ));
        }
        bounded(context, list(context, parse_arguments(args)?, false)).await
    }
}
#[async_trait]
impl ExecutableTool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        definition(true)
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        bounded(context, list(context, parse_arguments(args)?, true)).await
    }
}
async fn list(
    context: &ToolContext,
    args: Arguments,
    recursive: bool,
) -> Result<ToolSuccess, ToolFailure> {
    let limit = page_limit(context)?;
    if args.path.len() > MAX_PATH_BYTES {
        return Err(failure("invalid_arguments", "path exceeds 4096 bytes"));
    }
    if !(1..=1000).contains(&args.limit) || recursive != args.pattern.is_some() {
        return Err(failure(
            "invalid_arguments",
            "limit must be 1..1000; only glob requires pattern",
        ));
    }
    let paths = WorkspacePaths::from_context(context);
    let root = paths.resolve_existing(&args.path)?;
    if root.to_str().is_none() {
        return Err(failure("unsupported_path", "Tool paths require UTF-8"));
    }
    if !recursive && !root.is_dir() {
        return Err(failure(
            "not_a_directory",
            "list_directory requires a directory",
        ));
    }
    let query = key(&(
        recursive,
        &root,
        &args.pattern,
        args.hidden,
        args.respect_gitignore,
    ));
    let after = args
        .cursor
        .as_deref()
        .map(|s| Cursor::decode(s, &query))
        .transpose()?;
    let matcher = args.pattern.as_deref().map(tree::glob).transpose()?;
    let tree = tree::enumerate(
        context,
        &root,
        recursive,
        args.hidden,
        args.respect_gitignore,
    )
    .await?;
    let root_is_file = root.is_file();
    let entries = tree
        .entries
        .into_iter()
        .filter(|entry| {
            let relative = context.workspace_root().join(&entry.path);
            let relative = if root_is_file {
                relative.file_name().map(Path::new).expect("file name")
            } else {
                relative.strip_prefix(&root).expect("scoped result")
            };
            after.as_ref().is_none_or(|c| entry.path > c.path)
                && matcher.as_ref().is_none_or(|m| m.is_match(relative))
        })
        .collect::<Vec<_>>();
    let make = |count: usize| {
        let has_more = count < entries.len();
        json!({"path":paths.relative_path(&root),"entries":&entries[..count],"has_more":has_more,"visited_entries":tree.visited,
            "next_cursor":if has_more && count > 0 {Some(Cursor::new(&query,entries[count-1].path.clone(),0,1,None).encode())}else{None}})
    };
    let (mut low, mut high) = (0, args.limit.min(entries.len()));
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if make(mid).to_string().len() <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    if low == 0 && !entries.is_empty() {
        return Err(too_small());
    }
    success(make(low), limit)
}
