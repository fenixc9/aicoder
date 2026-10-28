//! Validated HTTP profiles and deterministic environment resolution.
use crate::credentials::{BearerToken, validate_variable};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityProfile {
    #[default]
    Generic,
    DeepseekThinking,
    DeepseekChat,
}
impl FromStr for CompatibilityProfile {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "generic" => Ok(Self::Generic),
            "deepseek-thinking" => Ok(Self::DeepseekThinking),
            "deepseek-chat" => Ok(Self::DeepseekChat),
            _ => Err(invalid(
                "compatibility must be generic, deepseek-thinking or deepseek-chat",
            )),
        }
    }
}
use aicoder_core::{
    model::{ProviderError, ProviderErrorKind},
    redaction::sanitize_text,
};
use anyhow::{Context, Result};
use reqwest::{ClientBuilder, Url};
use serde::{Deserialize, Serialize, Serializer};
use std::{
    fmt,
    str::FromStr,
    time::{Duration, Instant},
};

#[derive(Clone, Default, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    Auto,
    Disabled,
    Explicit {
        url: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
    },
}
impl ProxyMode {
    pub fn explicit(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        let parsed = proxy_url(&url)?;
        let username = (!parsed.username().is_empty())
            .then(|| decode_userinfo(parsed.username()))
            .transpose()?;
        let password = parsed.password().map(decode_userinfo).transpose()?;
        let mut endpoint = parsed;
        let _ = endpoint.set_username("");
        let _ = endpoint.set_password(None);
        Ok(Self::Explicit {
            url: endpoint.into(),
            username,
            password,
        })
    }
    pub(crate) fn apply(&self, builder: ClientBuilder) -> Result<ClientBuilder> {
        match self {
            Self::Auto => Ok(builder),
            Self::Disabled => Ok(builder.no_proxy()),
            Self::Explicit {
                url,
                username,
                password,
            } => {
                let parsed = proxy_url(url)?;
                let embedded_user = (!parsed.username().is_empty())
                    .then(|| decode_userinfo(parsed.username()))
                    .transpose()?;
                let embedded_password = parsed.password().map(decode_userinfo).transpose()?;
                if (embedded_user.is_some() || embedded_password.is_some())
                    && (username.is_some() || password.is_some())
                {
                    return Err(invalid(
                        "proxy credentials must use either URL userinfo or explicit fields",
                    ));
                }
                let user = username.as_ref().or(embedded_user.as_ref());
                let pass = password.as_ref().or(embedded_password.as_ref());
                if pass.is_some() && user.is_none() {
                    return Err(invalid("proxy password requires a username"));
                }
                let mut endpoint = parsed;
                let _ = endpoint.set_username("");
                let _ = endpoint.set_password(None);
                let mut proxy = reqwest::Proxy::all(endpoint)
                    .map_err(|_| invalid("invalid HTTP proxy endpoint"))?;
                if let Some(user) = user {
                    proxy = proxy.basic_auth(user, pass.map_or("", String::as_str));
                }
                Ok(builder.no_proxy().proxy(proxy))
            }
        }
    }
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut text = text.to_string();
        if let Self::Explicit {
            url,
            username,
            password,
        } = self
        {
            for secret in [username.as_deref(), password.as_deref()]
                .into_iter()
                .flatten()
                .filter(|v| !v.is_empty())
            {
                text = text.replace(secret, "[REDACTED]");
            }
            if let Ok(parsed) = Url::parse(url) {
                for secret in [Some(parsed.username()), parsed.password()]
                    .into_iter()
                    .flatten()
                    .filter(|v| !v.is_empty())
                {
                    text = text.replace(secret, "[REDACTED]");
                    if let Ok(decoded) = decode_userinfo(secret)
                        && !decoded.is_empty()
                    {
                        text = text.replace(&decoded, "[REDACTED]");
                    }
                }
            }
        }
        sanitize_text(&text, 8192)
    }
}
impl fmt::Debug for ProxyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("Auto"),
            Self::Disabled => f.write_str("Disabled"),
            Self::Explicit { url, .. } => f
                .debug_struct("Explicit")
                .field("url", &public_url(url))
                .field("credentials", &"[REDACTED]")
                .finish(),
        }
    }
}
impl Serialize for ProxyMode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serde_json::json!({"mode":"auto"}).serialize(s),
            Self::Disabled => serde_json::json!({"mode":"disabled"}).serialize(s),
            Self::Explicit { url, .. } => {
                serde_json::json!({"mode":"explicit","url":public_url(url)}).serialize(s)
            }
        }
    }
}
impl FromStr for ProxyMode {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "disabled" | "no-proxy" | "none" => Ok(Self::Disabled),
            _ => Self::explicit(value),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub provider_id: String,
    #[serde(serialize_with = "serialize_url")]
    pub base_url: String,
    #[serde(skip_serializing)]
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    pub model: String,
    /// Entire HTTP response timeout in seconds; separate from the executor's turn budget.
    pub timeout: u64,
    pub connect_timeout: u64,
    pub max_retries: u32,
    pub proxy: ProxyMode,
    pub stream_usage: bool,
    pub compatibility: CompatibilityProfile,
    /// Maximum successful response bytes, including SSE framing when streaming.
    pub max_response_bytes: usize,
}
impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            provider_id: "openai-compatible".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            api_key_env: None,
            model: "gpt-4o".into(),
            timeout: 120,
            connect_timeout: 10,
            max_retries: 3,
            proxy: ProxyMode::Auto,
            stream_usage: true,
            compatibility: CompatibilityProfile::Generic,
            max_response_bytes: 32 * 1024 * 1024,
        }
    }
}
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("provider_id", &sanitize_text(&self.provider_id, 128))
            .field("base_url", &public_url(&self.base_url))
            .field("api_key", &"[REDACTED]")
            .field(
                "api_key_env",
                &self.api_key_env.as_ref().map(|s| sanitize_text(s, 128)),
            )
            .field("model", &sanitize_text(&self.model, 256))
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("max_retries", &self.max_retries)
            .field("proxy", &self.proxy)
            .field("stream_usage", &self.stream_usage)
            .field("compatibility", &self.compatibility)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}
impl ClientConfig {
    pub(crate) fn endpoint(&self) -> Result<Url> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|_| invalid("base_url must be an absolute HTTP(S) URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "base_url must be an HTTP(S) endpoint without userinfo, query or fragment",
            ));
        }
        url.set_path(&format!(
            "{}/chat/completions",
            url.path().trim_end_matches('/')
        ));
        Ok(url)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.endpoint()?;
        if self.provider_id.trim().is_empty() {
            return Err(invalid("provider_id must not be empty"));
        }
        if self.model.trim().is_empty() {
            return Err(invalid("model must not be empty"));
        }
        if self.timeout == 0 || self.connect_timeout == 0 {
            return Err(invalid(
                "HTTP and connect timeouts must be positive seconds",
            ));
        }
        for seconds in [self.timeout, self.connect_timeout] {
            if Instant::now()
                .checked_add(Duration::from_secs(seconds))
                .is_none()
            {
                return Err(invalid("HTTP timeout is too large"));
            }
        }
        if self.max_response_bytes == 0 {
            return Err(invalid("max_response_bytes must be positive"));
        }
        if let Some(variable) = &self.api_key_env {
            validate_variable(variable)?;
        }
        Ok(())
    }
    pub fn from_env(model: Option<&str>) -> Result<Self> {
        Self::from_environment(model, |name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(invalid("provider environment contains a non-Unicode value")),
        })
    }
    /// Pure resolver, suitable for tests or a host-owned environment snapshot.
    pub fn from_environment(
        model: Option<&str>,
        lookup: impl Fn(&str) -> Result<Option<String>>,
    ) -> Result<Self> {
        Self::resolve_environment(model, lookup, true)
    }
    /// Resolve a non-secret routing descriptor without requiring credentials to exist yet.
    pub fn descriptor_from_environment(
        model: Option<&str>,
        lookup: impl Fn(&str) -> Result<Option<String>>,
    ) -> Result<Self> {
        Self::resolve_environment(model, lookup, false)
    }
    fn resolve_environment(
        model: Option<&str>,
        lookup: impl Fn(&str) -> Result<Option<String>>,
        require_key: bool,
    ) -> Result<Self> {
        let get = |name: &str| -> Result<Option<String>> {
            Ok(lookup(name)?.filter(|value| !value.trim().is_empty()))
        };
        let generic_base = first(&get, &["AICODER_API_BASE", "OPENAI_API_BASE"])?;
        let explicit_ref = get("AICODER_API_KEY_ENV")?;
        let (key, variable, family) = if let Some(variable) = explicit_ref {
            validate_variable(&variable)?;
            let key = if require_key {
                get(&variable)?.context("configured API key environment variable is missing")?
            } else {
                String::new()
            };
            (key, variable, "custom")
        } else if let Some(key) = get("AICODER_API_KEY")? {
            (key, "AICODER_API_KEY".into(), "custom")
        } else if let Some(key) = get("OPENAI_API_KEY")? {
            (key, "OPENAI_API_KEY".into(), "openai")
        } else if let Some(key) = get("DEEPSEEK_API_KEY")? {
            (key, "DEEPSEEK_API_KEY".into(), "deepseek")
        } else if !require_key {
            (String::new(), "OPENAI_API_KEY".into(), "openai")
        } else {
            return Err(invalid(
                "No API key found. Set OPENAI_API_KEY or DEEPSEEK_API_KEY, or configure AICODER_API_KEY_ENV with AICODER_API_BASE",
            ));
        };
        if require_key {
            BearerToken::new(key.clone())?;
        }
        let base_url = match generic_base {
            Some(base) => base,
            None => match family {
                "openai" => Self::default().base_url,
                "deepseek" => {
                    get("DEEPSEEK_API_BASE")?.unwrap_or_else(|| "https://api.deepseek.com".into())
                }
                _ => {
                    return Err(invalid(
                        "custom credentials require AICODER_API_BASE or OPENAI_API_BASE",
                    ));
                }
            },
        };
        let host = reqwest::Url::parse(&base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string));
        let deepseek = family == "deepseek" || host.as_deref() == Some("api.deepseek.com");
        let compatibility = match get("AICODER_COMPATIBILITY")? {
            Some(value) => value.parse()?,
            None if deepseek => CompatibilityProfile::DeepseekThinking,
            None => CompatibilityProfile::Generic,
        };
        let model = match model {
            Some(model) => model.to_string(),
            None => first(
                &get,
                if deepseek || compatibility != CompatibilityProfile::Generic {
                    &["AICODER_MODEL", "OPENAI_MODEL", "DEEPSEEK_MODEL"]
                } else {
                    &["AICODER_MODEL", "OPENAI_MODEL"]
                },
            )?
            .or_else(|| match family {
                _ if deepseek || compatibility != CompatibilityProfile::Generic => {
                    Some("deepseek-flash".into())
                }
                _ if host.as_deref() == Some("api.openai.com") => Some(Self::default().model),
                _ => None,
            })
            .context("custom endpoints require an explicit model or AICODER_MODEL")?,
        };
        let mut config = Self {
            provider_id: get("AICODER_PROVIDER_ID")?.unwrap_or_else(|| {
                if deepseek {
                    "deepseek".into()
                } else if host.as_deref() == Some("api.openai.com") {
                    "openai".into()
                } else {
                    "openai-compatible".into()
                }
            }),
            base_url,
            model,
            api_key: if require_key { key } else { String::new() },
            api_key_env: Some(variable),
            compatibility,
            ..Self::default()
        };
        if let Some(value) = get("AICODER_PROXY")? {
            config.proxy = value.parse()?;
        }
        macro_rules! number {
            ($name:literal,$field:ident) => {
                if let Some(value) = get($name)? {
                    config.$field = value
                        .parse()
                        .map_err(|_| invalid(concat!($name, " must be a nonnegative integer")))?;
                }
            };
        }
        number!("AICODER_HTTP_TIMEOUT", timeout);
        number!("AICODER_CONNECT_TIMEOUT", connect_timeout);
        number!("AICODER_MAX_RETRIES", max_retries);
        number!("AICODER_MAX_RESPONSE_BYTES", max_response_bytes);
        if let Some(value) = get("AICODER_STREAM_USAGE")? {
            config.stream_usage = value
                .parse()
                .map_err(|_| invalid("AICODER_STREAM_USAGE must be true or false"))?;
        }
        config.validate()?;
        Ok(config)
    }
}
fn proxy_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| invalid("invalid proxy URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(invalid(
            "proxy must be an HTTP(S) URL without path, query or fragment",
        ));
    }
    Ok(url)
}
fn decode_userinfo(text: &str) -> Result<String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(invalid("invalid encoded proxy credentials"));
            }
            let digit = |b: u8| char::from(b).to_digit(16).map(|v| v as u8);
            let high = digit(bytes[i + 1]).context("invalid encoded proxy credentials")?;
            let low = digit(bytes[i + 2]).context("invalid encoded proxy credentials")?;
            result.push(high * 16 + low);
            i += 3;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(result).map_err(|_| invalid("proxy credentials must be Unicode"))
}
pub(crate) fn public_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "[invalid URL]".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    sanitize_text(url.as_str(), 2048)
}
fn serialize_url<S: Serializer>(value: &str, s: S) -> Result<S::Ok, S::Error> {
    public_url(value).serialize(s)
}
pub(crate) fn invalid(message: &str) -> anyhow::Error {
    ProviderError::new(
        "openai-compatible",
        ProviderErrorKind::InvalidRequest,
        message,
    )
    .into()
}

fn first(get: &impl Fn(&str) -> Result<Option<String>>, names: &[&str]) -> Result<Option<String>> {
    for name in names {
        if let Some(value) = get(name)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

pub(crate) fn system_proxy_secrets(lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut secrets = Vec::new();
    for name in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        if let Some(value) = lookup(name)
            && let Ok(url) = Url::parse(&value)
        {
            for value in [Some(url.username()), url.password()]
                .into_iter()
                .flatten()
                .filter(|value| !value.is_empty())
            {
                for secret in [Some(value.to_string()), decode_userinfo(value).ok()]
                    .into_iter()
                    .flatten()
                {
                    if !secret.is_empty() && !secrets.contains(&secret) {
                        secrets.push(secret);
                    }
                }
            }
        }
    }
    secrets
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resolve(model: Option<&str>, env: &[(&str, &str)]) -> Result<ClientConfig> {
        ClientConfig::from_environment(model, |name| {
            Ok(env
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string()))
        })
    }
    #[test]
    fn native_keys_select_their_own_endpoint_and_model() {
        let openai = resolve(
            None,
            &[
                ("OPENAI_API_KEY", "openai-test"),
                ("DEEPSEEK_API_BASE", "https://other.invalid"),
            ],
        )
        .unwrap();
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
        assert_eq!(openai.model, "gpt-4o");
        assert_eq!(openai.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        let deepseek = resolve(None, &[("DEEPSEEK_API_KEY", "deepseek-test")]).unwrap();
        assert_eq!(deepseek.base_url, "https://api.deepseek.com");
        assert_eq!(deepseek.model, "deepseek-flash");
        assert!(resolve(None, &[("ANTHROPIC_API_KEY", "never-send-this")]).is_err());
    }
    #[test]
    fn custom_credentials_require_an_explicit_endpoint_and_model() {
        assert!(resolve(None, &[("AICODER_API_KEY", "custom-test")]).is_err());
        assert!(
            resolve(
                None,
                &[
                    ("AICODER_API_KEY", "custom-test"),
                    ("AICODER_API_BASE", "https://custom.invalid")
                ]
            )
            .is_err()
        );
        let config = resolve(
            Some("custom"),
            &[
                ("AICODER_API_KEY_ENV", "CUSTOM_TOKEN"),
                ("CUSTOM_TOKEN", "custom-test"),
                ("AICODER_API_BASE", "https://custom.invalid/v1"),
                ("OPENAI_API_KEY", "unused"),
            ],
        )
        .unwrap();
        assert_eq!(config.api_key, "custom-test");
        assert_eq!(config.api_key_env.as_deref(), Some("CUSTOM_TOKEN"));
        assert!(
            resolve(
                Some("custom"),
                &[
                    ("AICODER_API_KEY_ENV", "MISSING"),
                    ("AICODER_API_BASE", "https://custom.invalid")
                ]
            )
            .is_err()
        );
    }
    #[test]
    fn higher_priority_model_and_endpoint_do_not_read_overridden_values() {
        let config = ClientConfig::from_environment(Some("explicit"), |name| match name {
            "AICODER_API_BASE" => Ok(Some("https://explicit.invalid".into())),
            "OPENAI_API_KEY" => Ok(Some("test-key".into())),
            "OPENAI_API_BASE" | "AICODER_MODEL" | "OPENAI_MODEL" => {
                panic!("read overridden variable {name}")
            }
            _ => Ok(None),
        })
        .unwrap();
        assert_eq!(config.model, "explicit");
        assert_eq!(config.base_url, "https://explicit.invalid");
        let config = resolve(
            None,
            &[
                ("DEEPSEEK_API_KEY", "test-key"),
                ("AICODER_MODEL", "chosen"),
                ("DEEPSEEK_MODEL", "unused"),
            ],
        )
        .unwrap();
        assert_eq!(config.model, "chosen");
    }
    #[test]
    fn transport_environment_is_validated_without_exposing_values() {
        let config = resolve(
            None,
            &[
                ("OPENAI_API_KEY", "test-key"),
                ("AICODER_PROXY", "no-proxy"),
                ("AICODER_HTTP_TIMEOUT", "30"),
                ("AICODER_CONNECT_TIMEOUT", "5"),
                ("AICODER_MAX_RETRIES", "0"),
                ("AICODER_STREAM_USAGE", "false"),
                ("AICODER_MAX_RESPONSE_BYTES", "4096"),
            ],
        )
        .unwrap();
        assert!(matches!(config.proxy, ProxyMode::Disabled));
        assert_eq!(config.timeout, 30);
        assert_eq!(config.connect_timeout, 5);
        assert_eq!(config.max_retries, 0);
        assert!(!config.stream_usage);
        assert_eq!(config.max_response_bytes, 4096);
        for (name, value) in [
            ("AICODER_HTTP_TIMEOUT", "0"),
            ("AICODER_STREAM_USAGE", "sometimes"),
            ("AICODER_MAX_RESPONSE_BYTES", "0"),
        ] {
            assert!(resolve(None, &[("OPENAI_API_KEY", "test-key"), (name, value)]).is_err());
        }
    }
    #[test]
    fn endpoints_are_normalized_and_invalid_auth_urls_are_rejected() {
        let mut config = ClientConfig {
            base_url: "https://example.com/prefix/v1///".into(),
            ..Default::default()
        };
        assert_eq!(
            config.endpoint().unwrap().as_str(),
            "https://example.com/prefix/v1/chat/completions"
        );
        for url in [
            "file:///tmp/key",
            "https://user:password@example.com/v1",
            "https://example.com/v1?api_key=secret",
            "https://example.com/v1#fragment",
        ] {
            config.base_url = url.into();
            assert!(config.validate().is_err());
        }
    }
    #[test]
    fn credentials_are_excluded_from_profile_debug_and_serialization() {
        let proxy =
            ProxyMode::explicit("http://proxy-user:proxy%2Dpassword@127.0.0.1:8080").unwrap();
        let config = ClientConfig {
            api_key: "plain-private-token".into(),
            api_key_env: Some("SERVICE_TOKEN".into()),
            proxy,
            ..Default::default()
        };
        for snapshot in [
            format!("{config:?}"),
            serde_json::to_string(&config).unwrap(),
        ] {
            for secret in [
                "plain-private-token",
                "proxy-user",
                "proxy-password",
                "proxy%2Dpassword",
            ] {
                assert!(!snapshot.contains(secret), "leaked {secret}");
            }
        }
        let restored: ClientConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(restored.api_key.is_empty());
        assert_eq!(restored.api_key_env.as_deref(), Some("SERVICE_TOKEN"));
    }
    #[test]
    fn proxy_modes_are_explicit_and_unsupported_schemes_fail() {
        assert!(matches!(
            "auto".parse::<ProxyMode>().unwrap(),
            ProxyMode::Auto
        ));
        assert!(matches!(
            "disabled".parse::<ProxyMode>().unwrap(),
            ProxyMode::Disabled
        ));
        for url in [
            "socks5://localhost:1080",
            "http://localhost:8080/path",
            "http://localhost:8080?password=secret",
        ] {
            assert!(ProxyMode::explicit(url).is_err());
        }
    }

    #[test]
    fn deepseek_profiles_declare_their_mode_and_custom_gateways_need_a_model() {
        let config = resolve(
            None,
            &[
                ("OPENAI_API_KEY", "test"),
                ("OPENAI_API_BASE", "https://api.deepseek.com"),
            ],
        )
        .unwrap();
        assert_eq!(config.model, "deepseek-flash");
        assert_eq!(config.compatibility, CompatibilityProfile::DeepseekThinking);
        let config = resolve(
            None,
            &[
                ("DEEPSEEK_API_KEY", "test"),
                ("AICODER_COMPATIBILITY", "deepseek-chat"),
            ],
        )
        .unwrap();
        assert_eq!(config.compatibility, CompatibilityProfile::DeepseekChat);
        assert!(
            resolve(
                None,
                &[
                    ("OPENAI_API_KEY", "test"),
                    ("OPENAI_API_BASE", "https://custom.invalid")
                ]
            )
            .is_err()
        );
    }
    #[test]
    fn system_proxy_credential_snapshot_includes_decoded_userinfo() {
        let secrets = system_proxy_secrets(|name| {
            (name == "HTTPS_PROXY")
                .then(|| "http://proxy-user:private%2Dpassword@localhost:8080".into())
        });
        assert!(secrets.iter().any(|value| value == "proxy-user"));
        assert!(secrets.iter().any(|value| value == "private-password"));
    }
}
