//! Request-time credentials, independent of model execution and persisted settings.
use aicoder_core::{
    model::{ProviderError, ProviderErrorKind},
    redaction::sanitize_text,
};
use async_trait::async_trait;
use reqwest::header::HeaderValue;
use std::fmt;

#[derive(Clone)]
pub struct BearerToken(String);
impl BearerToken {
    /// Explicit access for authentication adapters; Debug and configuration snapshots redact secrets.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderError> {
        let value = value.into();
        if value.is_empty() {
            return Err(auth("API key is required"));
        }
        if value
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(auth(
                "API key must not contain whitespace or control characters",
            ));
        }
        HeaderValue::from_str(&format!("Bearer {value}"))
            .map_err(|_| auth("API key is not a valid bearer credential"))?;
        Ok(Self(value))
    }
    pub(crate) fn header(&self) -> HeaderValue {
        let mut header =
            HeaderValue::from_str(&format!("Bearer {}", self.0)).expect("validated credential");
        header.set_sensitive(true);
        header
    }
    pub(crate) fn redact(&self, text: &str, truncated: bool) -> String {
        sanitize_text(&redact_value(text, &self.0, truncated), 8192)
    }
}
impl fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerToken([REDACTED])")
    }
}

#[async_trait]
pub trait CredentialProvider: Send + Sync {
    /// May refresh credentials asynchronously. The token is requested for each HTTP attempt.
    async fn bearer_token(&self) -> Result<BearerToken, ProviderError>;
}
pub struct StaticCredential(BearerToken);
impl StaticCredential {
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderError> {
        Ok(Self(BearerToken::new(value)?))
    }
}
#[async_trait]
impl CredentialProvider for StaticCredential {
    async fn bearer_token(&self) -> Result<BearerToken, ProviderError> {
        Ok(self.0.clone())
    }
}
#[derive(Debug, Clone)]
pub struct EnvironmentCredential {
    variable: String,
}
impl EnvironmentCredential {
    pub fn new(variable: impl Into<String>) -> Result<Self, ProviderError> {
        let variable = variable.into();
        validate_variable(&variable)?;
        Ok(Self { variable })
    }
}
#[async_trait]
impl CredentialProvider for EnvironmentCredential {
    async fn bearer_token(&self) -> Result<BearerToken, ProviderError> {
        let value = std::env::var(&self.variable).map_err(|_| {
            auth("configured credential environment variable is missing or not Unicode")
        })?;
        BearerToken::new(value)
    }
}
#[async_trait]
impl<F> CredentialProvider for F
where
    F: Fn() -> Result<BearerToken, ProviderError> + Send + Sync,
{
    async fn bearer_token(&self) -> Result<BearerToken, ProviderError> {
        self()
    }
}
pub(crate) fn validate_variable(name: &str) -> Result<(), ProviderError> {
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(auth(
            "credential environment variable must be a valid identifier",
        ));
    }
    Ok(())
}
fn auth(message: &str) -> ProviderError {
    ProviderError::new(
        "openai-compatible",
        ProviderErrorKind::Authentication,
        message,
    )
}

pub(crate) fn redact_value(text: &str, secret: &str, truncated: bool) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    let mut text = text.replace(secret, "[REDACTED]");
    if truncated {
        for n in (1..=secret.len().min(text.len())).rev() {
            if secret.is_char_boundary(n) && text.ends_with(&secret[..n]) {
                text.truncate(text.len() - n);
                text.push_str("[REDACTED]");
                break;
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_redacted_even_when_a_body_ends_mid_token() {
        let token = BearerToken::new("plain-private-token-value").unwrap();
        assert!(!format!("{token:?}").contains("plain-private"));
        assert_eq!(token.redact("plain-private", true), "[REDACTED]");
        assert!(
            !token
                .redact("server echoed plain-private-token-value", false)
                .contains("plain-private")
        );
        assert!(token.header().is_sensitive());
        assert!(BearerToken::new("secret\ninjected").is_err());
    }
}
