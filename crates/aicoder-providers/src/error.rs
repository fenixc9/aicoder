use aicoder_core::{model::ProviderError, redaction::sanitize_text};
use reqwest::StatusCode;
use serde_json::Value;

/// Accept nested OpenAI/DeepSeek errors, flat compatible errors, string errors and plain text.
pub(crate) fn response_error(
    provider: &str,
    status: StatusCode,
    body: &str,
    attempt: u32,
    truncated: bool,
    redact: impl Fn(&str) -> String,
) -> ProviderError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let root = parsed.as_ref();
    let detail = root
        .and_then(|value| value.get("error"))
        .filter(|value| !value.is_null())
        .or(root);
    let scalar = |value: Option<&Value>| -> Option<String> {
        value.and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    let message = detail
        .and_then(|v| {
            v.get("message")
                .and_then(Value::as_str)
                .or_else(|| v.get("detail").and_then(Value::as_str))
                .or_else(|| v.as_str())
        })
        .unwrap_or(body);
    let message = sanitize_text(&redact(message), 3000);
    let legacy_type = detail
        .and_then(|v| v.get("error_type"))
        .and_then(Value::as_str);
    let mut formatted = if let Some(kind) = legacy_type {
        format!(
            "API error: {} - {message}",
            sanitize_text(&redact(kind), 256)
        )
    } else {
        format!("API error (status {status}) after {attempt} attempt(s): {message}")
    };
    if truncated {
        formatted.push_str("\n[response body truncated]");
    }
    let mut error = crate::model::http_error(status, sanitize_text(&formatted, 8192));
    error.provider = provider.to_string();
    error.code =
        scalar(detail.and_then(|v| v.get("code"))).map(|value| sanitize_text(&redact(&value), 256));
    error.parameter = scalar(detail.and_then(|v| v.get("param")))
        .map(|value| sanitize_text(&redact(&value), 256));
    error
}
