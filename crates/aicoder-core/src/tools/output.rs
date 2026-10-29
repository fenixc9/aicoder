use super::{ToolContext, ToolFailure, ToolSuccess};
use crate::{
    artifacts::{ArtifactRef, store_bytes},
    events::ToolExecutionOutcome,
};
use serde_json::{Value, json};
use std::io::Write;
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let available = self.limit.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(available)]);
        if bytes.len() > available {
            return Err(std::io::Error::other("output limit"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
/// Bounded serialization even when a custom tool returns an unexpectedly large Value.
async fn reduce(
    value: Value,
    context: &ToolContext,
    can_archive: bool,
) -> (Value, Option<ArtifactRef>, Option<&'static str>, bool) {
    let display = context.max_output_bytes();
    let cap = context.artifact_store().filter(|_| can_archive).map_or(
        display.saturating_add(1),
        |store| {
            (store.limits().max_artifact_bytes.min(usize::MAX as u64) as usize)
                .max(display.saturating_add(1))
        },
    );
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit: cap,
    };
    let complete = serde_json::to_writer(&mut writer, &value).is_ok();
    if complete && writer.bytes.len() <= display {
        return (value, None, None, false);
    }
    let mut artifact = None;
    let mut error = None;
    if let Some(store) = context.artifact_store() {
        if !can_archive {
            error = Some("reference_limit");
        } else if complete {
            match tokio::time::timeout(
                std::time::Duration::from_millis(250),
                store_bytes(store.as_ref(), "application/json", &writer.bytes, true),
            )
            .await
            {
                Ok(Ok(reference)) => artifact = Some(reference),
                Ok(Err(failure)) => error = Some(failure.code),
                Err(_) => error = Some("storage_timeout"),
            }
        } else {
            error = Some("too_large");
        }
    }
    let preview = preview(&writer.bytes, display);
    (preview, artifact, error, true)
}
fn preview(bytes: &[u8], capacity: usize) -> Value {
    let valid = std::str::from_utf8(bytes).unwrap_or_else(|error| {
        std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("JSON UTF-8 prefix")
    });
    let mut cost = 2usize;
    let mut end = 0;
    for (index, ch) in valid.char_indices() {
        let added = match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
            ch if ch < '\u{20}' => 6,
            _ => ch.len_utf8(),
        };
        if cost.saturating_add(added) > capacity {
            break;
        }
        cost += added;
        end = index + ch.len_utf8();
    }
    Value::String(valid[..end].to_string())
}
pub(crate) async fn success_envelope(
    mut success: ToolSuccess,
    context: &ToolContext,
) -> (Value, ToolExecutionOutcome) {
    success.artifacts.truncate(8);
    success
        .artifacts
        .retain(|reference| reference.validate().is_ok());
    let (output, artifact, error, cut) =
        reduce(success.output, context, success.artifacts.len() < 8).await;
    success.output = output;
    success.truncated |= cut;
    if let Some(artifact) = artifact {
        success.artifacts.push(artifact);
    }
    success.artifacts.truncate(8);
    success
        .artifacts
        .retain(|reference| reference.validate().is_ok());
    let outcome = ToolExecutionOutcome::Succeeded {
        output: success.output.clone(),
        truncated: success.truncated,
    };
    let mut envelope = json!({"ok":true,"output":success.output,"truncated":success.truncated});
    if !success.artifacts.is_empty() {
        envelope["artifacts"] = json!(success.artifacts);
    }
    if let Some(code) = error {
        envelope["artifact_error"] = json!(code);
    }
    if cut {
        envelope["preview_format"] = json!("json_prefix");
    }
    (envelope, outcome)
}
pub(crate) async fn bound_failure(mut failure: ToolFailure, context: &ToolContext) -> ToolFailure {
    if let Some(details) = failure.details.take() {
        failure.artifacts.truncate(8);
        failure
            .artifacts
            .retain(|reference| reference.validate().is_ok());
        let (details, artifact, error, cut) =
            reduce(details, context, failure.artifacts.len() < 8).await;
        failure.details = Some(if cut {
            let mut wrapper =
                json!({"preview":"","preview_format":"json_prefix","artifact_error":error});
            let available = context
                .max_output_bytes()
                .saturating_sub(wrapper.to_string().len())
                .saturating_add(2);
            wrapper["preview"] =
                preview(details.as_str().unwrap_or_default().as_bytes(), available);
            wrapper
        } else {
            details
        });
        if let Some(reference) = artifact {
            failure.artifacts.push(reference);
        }
    }
    failure.message = super::util::truncate_utf8(&failure.message, 2048).0;
    failure.code = super::util::truncate_utf8(&failure.code, 128).0;
    failure.artifacts.truncate(8);
    failure
        .artifacts
        .retain(|reference| reference.validate().is_ok());
    failure
}
