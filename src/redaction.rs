//! Helpers for keeping credentials and private payloads out of logs and errors.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

const REDACTED: &str = "[REDACTED]";

fn credential_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)(?:bearer\s+)[A-Za-z0-9._~+/=-]+",
            r"|\b(?:sk-[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9_]{8,}|AKIA[A-Z0-9]{16})\b",
            r"|\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b",
            r#"|\b(?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password)\s*[:=]\s*["']?[^\s,"'&}]+"#,
            r"|https?://[^/\s:@]+:[^@\s/]+@",
        ))
        .expect("credential redaction regex must be valid")
    })
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        key.as_str(),
        "authorization"
            | "proxy_authorization"
            | "api_key"
            | "apikey"
            | "access_token"
            | "refresh_token"
            | "password"
            | "passwd"
            | "secret"
            | "client_secret"
            | "private_key"
            | "cookie"
            | "set_cookie"
    ) || key.ends_with("_api_key")
        || key.ends_with("_access_token")
        || key.ends_with("_refresh_token")
        || key.ends_with("_password")
        || key.ends_with("_secret")
}

fn redact_patterns(input: &str) -> String {
    credential_pattern()
        .replace_all(input, REDACTED)
        .into_owned()
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_json_value(value);
            }
        }
        Value::String(value) => *value = redact_patterns(value),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }

    let mut end = max_bytes.min(input.len());
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_string()
}

/// Redacts known credentials, recursively sanitizes JSON, and bounds output size.
pub(crate) fn sanitize_text(input: &str, max_bytes: usize) -> String {
    let redacted = match serde_json::from_str::<Value>(input) {
        Ok(mut value) => {
            redact_json_value(&mut value);
            value.to_string()
        }
        Err(_) => redact_patterns(input),
    };
    truncate_utf8(&redacted, max_bytes)
}

/// Removes credentials from URL userinfo and sensitive query parameters.
pub(crate) fn sanitize_url(input: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(input) else {
        return sanitize_text(input, 2048);
    };

    if url.password().is_some() {
        let _ = url.set_password(Some(REDACTED));
    }

    let query = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if !query.is_empty() {
        url.set_query(None);
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            let value = if is_sensitive_key(&key) {
                REDACTED.to_string()
            } else {
                redact_patterns(&value)
            };
            pairs.append_pair(&key, &value);
        }
    }

    sanitize_text(url.as_str(), 2048)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_nested_json_without_hiding_usage_tokens() {
        let input = serde_json::json!({
            "api_key": "sk-super-secret-value",
            "nested": {
                "password": "hunter2",
                "authorization": "Bearer abc.def.ghi",
                "prompt_tokens": 100,
                "cached_tokens": 80,
                "max_tokens": 2048
            }
        })
        .to_string();

        let sanitized: Value = serde_json::from_str(&sanitize_text(&input, 4096)).unwrap();
        assert_eq!(sanitized["api_key"], REDACTED);
        assert_eq!(sanitized["nested"]["password"], REDACTED);
        assert_eq!(sanitized["nested"]["authorization"], REDACTED);
        assert_eq!(sanitized["nested"]["prompt_tokens"], 100);
        assert_eq!(sanitized["nested"]["cached_tokens"], 80);
        assert_eq!(sanitized["nested"]["max_tokens"], 2048);
    }

    #[test]
    fn sanitizes_credentials_embedded_in_plain_text() {
        let input = concat!(
            "Authorization: Bearer abc123.secret and api_key=plain-secret ",
            "at https://user:url-password@example.com/v1"
        );
        let sanitized = sanitize_text(input, 4096);
        assert!(!sanitized.contains("abc123.secret"));
        assert!(!sanitized.contains("plain-secret"));
        assert!(!sanitized.contains("url-password"));
        assert!(sanitized.contains(REDACTED));
    }

    #[test]
    fn sanitizes_url_password_and_query_credentials() {
        let sanitized =
            sanitize_url("https://user:password@example.com/v1?api_key=secret&prompt_tokens=100");
        assert!(!sanitized.contains("password"));
        assert!(!sanitized.contains("secret"));
        assert!(sanitized.contains("prompt_tokens=100"));
        assert!(sanitized.contains(REDACTED));
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        assert_eq!(sanitize_text("你好世界", 5), "你");
        assert_eq!(sanitize_text("hello", 0), "");
    }
}
