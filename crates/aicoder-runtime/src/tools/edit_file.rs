use std::ops::Range;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use aicoder_core::model::ToolDefinition;

use super::{
    ExecutableTool, PreparedTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess,
    atomic_file::{FilePlan, Target},
    util::parse_arguments,
};

const MAX_EDITS: usize = 32;
const MAX_EDIT_ARGUMENT_BYTES: usize = 1024 * 1024;

pub struct EditFileTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditFileArguments {
    path: String,
    edits: Vec<TextEdit>,
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextEdit {
    old_text: String,
    new_text: String,
}

struct PreparedEdit {
    range: Range<usize>,
    new_text: String,
}

#[async_trait]
impl ExecutableTool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: Some(
                "Atomically apply exact text replacements to one UTF-8 file. Every old_text must match exactly once in the original file and edits must not overlap."
                    .to_string(),
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "expected_revision": {"type":"string","description":"Revision from read_file; reject changes since that read"},
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_EDITS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {"type": "string", "minLength": 1},
                                "new_text": {"type": "string"}
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            }),
        }
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }

    async fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<Option<Box<dyn PreparedTool>>, ToolFailure> {
        let arguments: EditFileArguments = parse_arguments(arguments)?;
        validate_arguments(&arguments)?;
        let context = context.clone();
        let plan = tokio::task::spawn_blocking(move || prepare_file(&context, arguments))
            .await
            .map_err(|error| ToolFailure::new("preparation_failed", error.to_string()))??;
        Ok(Some(Box::new(plan)))
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        tokio::time::timeout(context.tool_timeout(), async {
            self.prepare(context, arguments)
                .await?
                .expect("file operation plan")
                .execute(context)
                .await
        })
        .await
        .map_err(|_| ToolFailure::new("timeout", "File edit exceeded its time limit"))?
    }
}

fn prepare_file(
    context: &ToolContext,
    arguments: EditFileArguments,
) -> Result<FilePlan, ToolFailure> {
    let target = Target::load(context, &arguments.path)?;
    target.expect(arguments.expected_revision.as_deref())?;
    let original = target
        .original
        .as_ref()
        .ok_or_else(|| ToolFailure::new("path_not_found", "File to edit does not exist"))?;
    let original = std::str::from_utf8(&original.bytes).map_err(|_| {
        ToolFailure::new("invalid_utf8", "edit_file only supports UTF-8 text files")
    })?;
    let prepared = prepare_edits(original, arguments.edits)?;
    let mut updated = original.to_string();
    for edit in prepared.iter().rev() {
        updated.replace_range(edit.range.clone(), &edit.new_text);
    }
    let output = json!({"edits_applied":prepared.len(),"bytes_before":original.len(),"bytes_after":updated.len()});
    FilePlan::new(context, target, updated.into_bytes(), output)
}

fn validate_arguments(arguments: &EditFileArguments) -> Result<(), ToolFailure> {
    if arguments.edits.is_empty() || arguments.edits.len() > MAX_EDITS {
        return Err(ToolFailure::new(
            "invalid_arguments",
            format!("edits must contain between 1 and {MAX_EDITS} entries"),
        ));
    }

    let mut argument_bytes = 0_usize;
    for (index, edit) in arguments.edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(ToolFailure::new(
                "invalid_arguments",
                format!("edits[{index}].old_text must not be empty"),
            ));
        }
        if edit.old_text == edit.new_text {
            return Err(ToolFailure::new(
                "no_change",
                format!("edits[{index}] does not change the file"),
            ));
        }
        argument_bytes = argument_bytes
            .checked_add(edit.old_text.len())
            .and_then(|size| size.checked_add(edit.new_text.len()))
            .ok_or_else(|| {
                ToolFailure::new("arguments_too_large", "Edit arguments are too large")
            })?;
    }
    if argument_bytes > MAX_EDIT_ARGUMENT_BYTES {
        return Err(ToolFailure::new(
            "arguments_too_large",
            format!("Edit text exceeds {MAX_EDIT_ARGUMENT_BYTES} byte limit"),
        ));
    }
    Ok(())
}

fn prepare_edits(original: &str, edits: Vec<TextEdit>) -> Result<Vec<PreparedEdit>, ToolFailure> {
    let mut prepared = Vec::with_capacity(edits.len());
    for (index, edit) in edits.into_iter().enumerate() {
        let matches = find_up_to_two(original, &edit.old_text);
        let start = match matches.as_slice() {
            [] => {
                return Err(ToolFailure::new(
                    "match_not_found",
                    format!("edits[{index}].old_text was not found"),
                )
                .with_details(json!({"edit_index": index})));
            }
            [start] => *start,
            _ => {
                return Err(ToolFailure::new(
                    "ambiguous_match",
                    format!(
                        "edits[{index}].old_text matched multiple locations; include more surrounding context"
                    ),
                )
                .with_details(json!({"edit_index": index, "matches": "at_least_2"})));
            }
        };
        prepared.push(PreparedEdit {
            range: start..start + edit.old_text.len(),
            new_text: edit.new_text,
        });
    }

    prepared.sort_by_key(|edit| edit.range.start);
    for (left, right) in prepared.iter().zip(prepared.iter().skip(1)) {
        if left.range.end > right.range.start {
            return Err(ToolFailure::new(
                "overlapping_edits",
                "Edit ranges overlap in the original file",
            ));
        }
    }
    Ok(prepared)
}

fn find_up_to_two(haystack: &str, needle: &str) -> Vec<usize> {
    let mut matches = Vec::with_capacity(2);
    let mut search_start = 0_usize;
    while search_start <= haystack.len() {
        let Some(relative) = haystack[search_start..].find(needle) else {
            break;
        };
        let start = search_start + relative;
        matches.push(start);
        if matches.len() == 2 {
            break;
        }
        let next_character_bytes = haystack[start..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        search_start = start.saturating_add(next_character_bytes);
    }
    matches
}
