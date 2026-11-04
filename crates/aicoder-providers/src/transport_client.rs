use crate::{BearerToken, ChatClient, ProviderEventSink, transport::*};
use aicoder_core::{
    events::AgentRawEvent,
    model::{ModelUsage, ProviderErrorKind},
};
use anyhow::Result;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
pub(crate) struct Prepared {
    pub payload: Value,
    pub headers: BTreeMap<String, String>,
    pub session: Option<Arc<Session>>,
}
pub(crate) struct Driver<'a> {
    pub config: &'a crate::ClientConfig,
    pub binding: Option<&'a TransportBinding>,
    pub proxy_secrets: &'a [String],
}
impl ChatClient {
    /// Explicit host assembly, never settings or restored history.
    pub fn with_transport_hooks(self, hooks: TransportHooks) -> Self {
        self.with_transport_binding(TransportBinding::new(hooks))
    }
    pub fn with_shared_transport_hooks(self, hooks: Arc<TransportHooks>) -> Self {
        self.with_transport_binding(TransportBinding::shared(hooks))
    }
    pub fn with_transport_binding(mut self, binding: TransportBinding) -> Self {
        self.transport_binding = binding.active().then_some(binding);
        self
    }
    pub fn transport_hook_catalog(&self) -> Vec<TransportHookInfo> {
        self.transport_binding
            .as_ref()
            .map(|b| b.catalog())
            .unwrap_or_default()
    }
    pub fn transport_payload_fields(&self) -> Vec<String> {
        self.transport_binding
            .as_ref()
            .map(|b| b.payload_fields())
            .unwrap_or_default()
    }
    pub fn transport_report(&self) -> TransportReport {
        self.transport_binding
            .as_ref()
            .map(|b| b.report())
            .unwrap_or_default()
    }
    fn transport_driver(&self) -> Driver<'_> {
        Driver {
            config: &self.config,
            binding: self.transport_binding.as_ref(),
            proxy_secrets: &self.proxy_secrets,
        }
    }
    pub(crate) async fn prepare_transport(
        &self,
        payload: Value,
        stream: bool,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<Prepared> {
        let mut headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
        if stream {
            headers.insert("accept".into(), "text/event-stream".into());
        }
        self.transport_driver()
            .prepare(payload, headers, events)
            .await
    }
    pub(crate) fn transport_admit(&self, session: Option<&Arc<Session>>) -> Result<()> {
        self.transport_driver().transport_admit(session)
    }
    pub(crate) async fn transport_ready(&self, session: Option<&Arc<Session>>) -> Result<()> {
        self.transport_driver().transport_ready(session).await
    }
    pub(crate) async fn observe_transport_response(
        &self,
        session: Option<&Arc<Session>>,
        attempt: u32,
        response: &reqwest::Response,
        token: &BearerToken,
    ) {
        self.transport_driver()
            .observe_transport_response(session, attempt, response, token)
            .await;
    }
    pub(crate) fn transport_usage_before_observation(
        &self,
        session: Option<&Arc<Session>>,
        usage: Option<&crate::wire::Usage>,
        events: Option<&dyn ProviderEventSink>,
    ) {
        self.transport_driver()
            .transport_usage_before_observation(session, usage, events);
    }
    pub(crate) async fn observe_transport_event(
        &self,
        session: Option<&Arc<Session>>,
        attempt: u32,
        sequence: u64,
        data: &str,
        token: &BearerToken,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<()> {
        self.transport_driver()
            .observe_transport_event(session, attempt, sequence, data, token, events)
            .await
    }
}
impl Driver<'_> {
    pub(crate) async fn prepare(
        &self,
        payload: Value,
        headers: BTreeMap<String, String>,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<Prepared> {
        let Some(binding) = self.binding else {
            return Ok(Prepared {
                payload,
                headers,
                session: None,
            });
        };
        let session = Session::new(
            &self.config.provider_id,
            &payload,
            binding.hooks.clone(),
            binding.report.clone(),
            Duration::from_secs(self.config.timeout),
        );
        let prepared = session.prepare(&payload, headers).await.and_then(|v| {
            session.ensure_active()?;
            Ok(v)
        });
        let (payload, headers) = match prepared {
            Ok(v) => v,
            Err(_) => {
                if let Some(events) = events {
                    events.emit(AgentRawEvent::UsageUpdated {
                        usage: ModelUsage::zero().into(),
                    });
                }
                return Err(aicoder_core::model::ProviderError::new(
                    &self.config.provider_id,
                    ProviderErrorKind::InvalidRequest,
                    "transport request transformation failed; inspect bounded hook diagnostics",
                )
                .into());
            }
        };
        // Cooperate with the owning run's cancellation/deadline after synchronous native work.
        tokio::task::yield_now().await;
        session.ensure_active()?;
        Ok(Prepared {
            payload,
            headers,
            session: Some(session),
        })
    }
    pub(crate) async fn transport_ready(&self, session: Option<&Arc<Session>>) -> Result<()> {
        if session.is_some() {
            tokio::task::yield_now().await;
        }
        self.transport_admit(session)
    }
    pub(crate) fn transport_admit(&self, session: Option<&Arc<Session>>) -> Result<()> {
        if let Some(session) = session {
            session.ensure_active().map_err(|_| {
                aicoder_core::model::ProviderError::new(
                    &self.config.provider_id,
                    ProviderErrorKind::Transport,
                    "transport request deadline elapsed; no new HTTP attempt was started",
                )
            })?;
        }
        Ok(())
    }
    pub(crate) async fn observe_transport_response(
        &self,
        session: Option<&Arc<Session>>,
        attempt: u32,
        response: &reqwest::Response,
        token: &BearerToken,
    ) {
        let Some(session) = session else {
            return;
        };
        let mut headers = BTreeMap::new();
        let mut bytes = 0usize;
        let mut complete = true;
        for (name, value) in response.headers() {
            let name = name.as_str();
            if matches!(
                name,
                "authorization"
                    | "proxy-authorization"
                    | "cookie"
                    | "set-cookie"
                    | "www-authenticate"
                    | "proxy-authenticate"
            ) {
                complete = false;
                continue;
            }
            let Ok(value) = value.to_str() else {
                complete = false;
                continue;
            };
            let value = self.redact_transport_text(token, value);
            bytes = bytes.saturating_add(name.len() + value.len());
            if headers.len() >= 64 || bytes > 16 * 1024 {
                complete = false;
                break;
            }
            headers.insert(name.into(), value);
        }
        session
            .response(attempt, response.status().as_u16(), &headers, complete)
            .await;
    }
    fn redact_transport_text(&self, token: &BearerToken, text: &str) -> String {
        let mut text = crate::credentials::redact_value(text, token.expose_secret(), false);
        text = self.config.proxy.redact(&text);
        for secret in self.proxy_secrets {
            text = crate::credentials::redact_value(&text, secret, false);
        }
        text
    }
    fn redact_transport_value(&self, token: &BearerToken, value: &mut Value) -> bool {
        let mut changed = false;
        match value {
            Value::String(s) => {
                let redacted = self.redact_transport_text(token, s);
                changed = redacted != *s;
                *s = redacted;
            }
            Value::Array(a) => {
                for v in a {
                    changed |= self.redact_transport_value(token, v);
                }
            }
            Value::Object(o) => {
                let old = std::mem::take(o);
                for (k, mut v) in old {
                    changed |= self.redact_transport_value(token, &mut v);
                    let key = self.redact_transport_text(token, &k);
                    changed |= key != k;
                    o.insert(key, v);
                }
            }
            _ => {}
        }
        changed
    }
    pub(crate) fn transport_usage_before_observation(
        &self,
        session: Option<&Arc<Session>>,
        usage: Option<&crate::wire::Usage>,
        events: Option<&dyn ProviderEventSink>,
    ) {
        if session.is_some()
            && let Some(usage) = usage.and_then(|v| crate::model::decode_usage(v).ok())
            && let Some(events) = events
        {
            events.emit(AgentRawEvent::UsageUpdated {
                usage: usage.into(),
            });
        }
    }
    pub(crate) async fn observe_transport_event(
        &self,
        session: Option<&Arc<Session>>,
        attempt: u32,
        sequence: u64,
        data: &str,
        token: &BearerToken,
        events: Option<&dyn ProviderEventSink>,
    ) -> Result<()> {
        let Some(session) = session else {
            return Ok(());
        };
        // Capture valid usage before awaiting observers, including a too-large event we omit.
        // Large events use the existing typed parser below; don't allocate a second JSON tree.
        if data.len() > MAX_TRANSPORT_EVENT_BYTES {
            session.omitted_event();
            return Ok(());
        }
        let mut value: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => {
                session.omitted_unparseable_event();
                return Ok(());
            }
        };
        if let Some(usage) = value
            .get("usage")
            .and_then(|v| serde_json::from_value::<crate::wire::Usage>(v.clone()).ok())
            .and_then(|v| crate::model::decode_usage(&v).ok())
            && let Some(events) = events
        {
            events.emit(AgentRawEvent::UsageUpdated {
                usage: usage.into(),
            });
        }
        let redacted = self.redact_transport_value(token, &mut value);
        if bound(&value, MAX_TRANSPORT_EVENT_BYTES).is_err() {
            session.omitted_event();
            return Ok(());
        }
        session.event(attempt, sequence, &value, redacted).await;
        Ok(())
    }
}

pub(crate) fn proxy_secrets(config: &crate::ClientConfig) -> Vec<String> {
    if matches!(config.proxy, crate::ProxyMode::Auto) {
        crate::config::system_proxy_secrets(|name| std::env::var(name).ok())
    } else {
        Vec::new()
    }
}
