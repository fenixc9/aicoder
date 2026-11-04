//! Explicit native HTTP extension stages. No settings/history can install these callbacks.
use aicoder_core::{TurnExecutionContext, extensions::HookSpec};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    panic::AssertUnwindSafe,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

pub const MAX_TRANSPORT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TRANSPORT_EVENT_BYTES: usize = 256 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportRequestInfo {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub stream: bool,
}
pub struct TransportHeadersInput<'a> {
    pub request: &'a TransportRequestInfo,
    pub headers: &'a BTreeMap<String, String>,
}
pub struct TransportPayloadInput<'a> {
    pub request: &'a TransportRequestInfo,
    pub payload: &'a Value,
}
pub struct TransportResponseInput<'a> {
    pub request: &'a TransportRequestInfo,
    pub attempt: u32,
    pub status: u16,
    pub headers: &'a BTreeMap<String, String>,
    pub headers_complete: bool,
}
pub struct TransportStreamInput<'a> {
    pub request: &'a TransportRequestInfo,
    pub attempt: u32,
    pub sequence: u64,
    pub data: &'a Value,
    pub credentials_redacted: bool,
}
#[derive(Clone)]
pub struct TransportCancellation(TurnExecutionContext);
impl TransportCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    pub async fn cancelled(&self) {
        let _ = self.0.cancelled().await;
    }
}
#[async_trait]
pub trait TransportHook: Send + Sync {
    async fn before_headers(
        &self,
        _: TransportHeadersInput<'_>,
        _: TransportCancellation,
    ) -> Result<BTreeMap<String, Option<String>>> {
        Ok(BTreeMap::new())
    }
    async fn before_payload(
        &self,
        _: TransportPayloadInput<'_>,
        _: TransportCancellation,
    ) -> Result<Option<Value>> {
        Ok(None)
    }
    async fn after_response(
        &self,
        _: TransportResponseInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        Ok(())
    }
    async fn stream_event(
        &self,
        _: TransportStreamInput<'_>,
        _: TransportCancellation,
    ) -> Result<()> {
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct TransportHookInfo {
    pub owner: String,
    pub spec: HookSpec,
}
struct Registration {
    info: TransportHookInfo,
    callback: Arc<dyn TransportHook>,
}
#[derive(Default)]
pub struct TransportHooks {
    hooks: Vec<Registration>,
    payload_fields: BTreeSet<String>,
}
fn name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn reserved_field(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "model"
            | "messages"
            | "input"
            | "prompt"
            | "instructions"
            | "tools"
            | "functions"
            | "tool_choice"
            | "function_call"
            | "stream"
            | "stream_options"
            | "temperature"
            | "top_p"
            | "max_tokens"
            | "max_completion_tokens"
            | "max_output_tokens"
            | "stop"
            | "seed"
            | "response_format"
            | "reasoning_effort"
            | "reasoning"
            | "thinking"
            | "enable_thinking"
            | "n"
            | "api_key"
            | "authorization"
            | "credentials"
            | "base_url"
            | "endpoint"
            | "url"
    )
}
fn protected_header(s: &str) -> bool {
    s.contains("authorization")
        || s.ends_with("api-key")
        || s.ends_with("auth-token")
        || matches!(
            s,
            "authorization"
                | "proxy-authorization"
                | "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "content-type"
                | "accept"
                | "cookie"
                | "set-cookie"
                | "openai-organization"
                | "openai-project"
        )
}
impl TransportHooks {
    pub fn new() -> Self {
        Self::default()
    }
    /// Only provider transport controls belong here; core-owned request fields always reject.
    pub fn allow_payload_field(&mut self, field: impl Into<String>) -> Result<&mut Self> {
        let field = field.into();
        ensure!(
            name(&field) && !reserved_field(&field) && self.payload_fields.len() < 64,
            "invalid transport payload field policy"
        );
        self.payload_fields.insert(field);
        Ok(self)
    }
    pub fn register(
        &mut self,
        owner: impl Into<String>,
        spec: HookSpec,
        callback: Arc<dyn TransportHook>,
    ) -> Result<&mut Self> {
        let owner = owner.into();
        ensure!(
            name(&owner)
                && name(&spec.id)
                && (Duration::from_millis(1)..=Duration::from_secs(30)).contains(&spec.timeout),
            "invalid transport hook specification"
        );
        ensure!(
            self.hooks.len() < 256
                && !self
                    .hooks
                    .iter()
                    .any(|h| h.info.owner == owner && h.info.spec.id == spec.id),
            "duplicate or excessive transport hook registration"
        );
        self.hooks.push(Registration {
            info: TransportHookInfo { owner, spec },
            callback,
        });
        self.hooks.sort_by_key(|h| h.info.spec.priority);
        Ok(self)
    }
    pub fn catalog(&self) -> Vec<TransportHookInfo> {
        self.hooks.iter().map(|h| h.info.clone()).collect()
    }
    pub fn payload_fields(&self) -> Vec<String> {
        self.payload_fields.iter().cloned().collect()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportDiagnostic {
    pub request_id: String,
    pub owner: String,
    pub hook: String,
    pub phase: String,
    pub failure: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransportReport {
    pub failures: Vec<TransportDiagnostic>,
    pub dropped: u64,
}
#[derive(Default)]
pub(crate) struct Diagnostics {
    rows: VecDeque<TransportDiagnostic>,
    dropped: u64,
}
impl Diagnostics {
    fn add(&mut self, row: TransportDiagnostic) {
        if self.rows.len() == 128 {
            self.rows.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.rows.push_back(row);
    }
    pub fn snapshot(&self) -> TransportReport {
        TransportReport {
            failures: self.rows.iter().cloned().collect(),
            dropped: self.dropped,
        }
    }
}
struct Cancel(TurnExecutionContext);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel("transport callback ended or dropped");
    }
}
pub struct TransportSession {
    pub info: TransportRequestInfo,
    deadline: tokio::time::Instant,
    expired_reported: AtomicBool,
    hooks: Arc<TransportHooks>,
    context: TurnExecutionContext,
    report: Arc<Mutex<Diagnostics>>,
}
pub(crate) type Session = TransportSession;
impl Drop for TransportSession {
    fn drop(&mut self) {
        self.context.cancel("transport request ended or dropped");
    }
}
impl TransportSession {
    /// Start an explicitly owned JSON-object driver scope with credential-redacted observations.
    pub fn start(
        provider: &str,
        payload: &Value,
        hooks: Arc<TransportHooks>,
        timeout: Duration,
    ) -> Result<Arc<Self>> {
        ensure!(
            !provider.is_empty()
                && provider.len() <= 256
                && !provider.chars().any(char::is_control),
            "invalid transport provider identity"
        );
        ensure!(
            (Duration::from_millis(1)..=Duration::from_secs(3600)).contains(&timeout),
            "invalid transport deadline"
        );
        Ok(Self::new(
            provider,
            payload,
            hooks,
            Arc::new(Mutex::new(Diagnostics::default())),
            timeout,
        ))
    }
    pub fn report(&self) -> TransportReport {
        self.report
            .lock()
            .expect("transport report poisoned")
            .snapshot()
    }
    pub fn cancel(&self, reason: &str) {
        self.context.cancel(reason);
    }
    pub(crate) fn new(
        provider: &str,
        payload: &Value,
        hooks: Arc<TransportHooks>,
        report: Arc<Mutex<Diagnostics>>,
        timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            deadline: tokio::time::Instant::now() + timeout,
            expired_reported: AtomicBool::new(false),
            info: TransportRequestInfo {
                id: aicoder_core::events::RunId::new().to_string(),
                provider: provider.into(),
                model: payload["model"].as_str().unwrap_or_default().into(),
                stream: payload["stream"].as_bool().unwrap_or(false),
            },
            hooks,
            context: TurnExecutionContext::new(),
            report,
        })
    }
    fn diagnostic(&self, registration: Option<&Registration>, phase: &str, failure: &str) {
        self.report
            .lock()
            .expect("transport report poisoned")
            .add(TransportDiagnostic {
                request_id: self.info.id.clone(),
                owner: registration
                    .map(|h| h.info.owner.clone())
                    .unwrap_or_default(),
                hook: registration
                    .map(|h| h.info.spec.id.clone())
                    .unwrap_or_default(),
                phase: phase.into(),
                failure: failure.into(),
            });
    }
    async fn invoke<T, F: Future<Output = Result<T>>>(
        &self,
        hook: &Registration,
        phase: &str,
        action: impl FnOnce(TransportCancellation) -> F,
    ) -> Result<T> {
        let child = self.context.child();
        let _guard = Cancel(child.clone());
        let work =
            AssertUnwindSafe(async { action(TransportCancellation(child)).await }).catch_unwind();
        let deadline = (tokio::time::Instant::now() + hook.info.spec.timeout).min(self.deadline);
        let result = match tokio::time::timeout_at(deadline, work).await {
            Ok(Ok(Ok(v))) if tokio::time::Instant::now() < deadline => return Ok(v),
            Ok(Ok(Ok(_))) => "timed_out",
            Ok(Ok(Err(_))) => "failed",
            Ok(Err(_)) => "panicked",
            Err(_) => "timed_out",
        };
        self.diagnostic(Some(hook), phase, result);
        anyhow::bail!(
            "transport hook {}/{} {phase} {result}",
            hook.info.owner,
            hook.info.spec.id
        )
    }
    pub fn ensure_active(&self) -> Result<()> {
        ensure!(
            tokio::time::Instant::now() < self.deadline && !self.context.is_cancelled(),
            "transport request deadline elapsed"
        );
        Ok(())
    }
    pub async fn prepare(
        &self,
        base: &Value,
        mut headers: BTreeMap<String, String>,
    ) -> Result<(Value, BTreeMap<String, String>)> {
        self.ensure_active()?;
        ensure!(base.is_object(), "transport payload must be an object");
        bound(base, MAX_TRANSPORT_PAYLOAD_BYTES)?;
        let mut payload = base.clone();
        let mut edited_bytes = 0usize;
        let work = async {
            for hook in &self.hooks.hooks {
                self.ensure_active()?;
                let edits = self
                    .invoke(hook, "headers", |c| {
                        hook.callback.before_headers(
                            TransportHeadersInput {
                                request: &self.info,
                                headers: &headers,
                            },
                            c,
                        )
                    })
                    .await?;
                bound(&edits, 16 * 1024)?;
                let mut names = BTreeSet::new();
                for (key, value) in edits {
                    let key = key.to_ascii_lowercase();
                    ensure!(
                        names.insert(key.clone()) && !protected_header(&key),
                        "transport hook attempted a protected/duplicate header edit"
                    );
                    reqwest::header::HeaderName::from_bytes(key.as_bytes())
                        .map_err(|_| anyhow::anyhow!("invalid transport header name"))?;
                    if let Some(value) = value {
                        reqwest::header::HeaderValue::from_str(&value)
                            .map_err(|_| anyhow::anyhow!("invalid transport header value"))?;
                        headers.insert(key, value);
                    } else {
                        headers.remove(&key);
                    }
                }
                ensure!(headers.len() <= 64, "transport header count exceeded");
                bound(&headers, 16 * 1024)?;
            }
            for hook in &self.hooks.hooks {
                self.ensure_active()?;
                if let Some(replacement) = self
                    .invoke(hook, "payload", |c| {
                        hook.callback.before_payload(
                            TransportPayloadInput {
                                request: &self.info,
                                payload: &payload,
                            },
                            c,
                        )
                    })
                    .await?
                {
                    bound(&replacement, MAX_TRANSPORT_PAYLOAD_BYTES)?;
                    let object = replacement
                        .as_object()
                        .ok_or_else(|| anyhow::anyhow!("transport payload must be an object"))?;
                    let anchor = base
                        .as_object()
                        .ok_or_else(|| anyhow::anyhow!("invalid base request body"))?;
                    ensure!(
                        anchor.iter().all(|(k, v)| object.get(k) == Some(v))
                            && object.keys().all(|k| anchor.contains_key(k)
                                || (self.hooks.payload_fields.contains(k) && !reserved_field(k))),
                        "transport payload changed core fields or unapproved extensions"
                    );
                    let extra = object
                        .iter()
                        .filter(|(k, _)| !anchor.contains_key(*k))
                        .collect::<BTreeMap<_, _>>();
                    edited_bytes = edited_bytes.saturating_add(bound(&extra, 64 * 1024)?);
                    ensure!(
                        edited_bytes <= 256 * 1024,
                        "transport payload aggregate budget exceeded"
                    );
                    payload = replacement;
                }
            }
            Ok::<_, anyhow::Error>((payload, headers))
        };
        let result = tokio::time::timeout_at(
            (tokio::time::Instant::now() + Duration::from_secs(30)).min(self.deadline),
            work,
        )
        .await
        .map_err(|_| anyhow::anyhow!("transport preparation timed out"))
        .and_then(|r| r);
        if result.is_err() {
            self.diagnostic(None, "prepare", "invalid_edits_or_deadline");
        }
        result
    }

    fn observe_active(&self, phase: &str) -> bool {
        if self.ensure_active().is_ok() {
            true
        } else {
            if !self.expired_reported.swap(true, Ordering::SeqCst) {
                self.diagnostic(None, phase, "observation_deadline_elapsed");
            }
            false
        }
    }
    pub async fn response(
        &self,
        attempt: u32,
        status: u16,
        headers: &BTreeMap<String, String>,
        complete: bool,
    ) {
        if headers.len() > 64 || bound(headers, 16 * 1024).is_err() {
            self.diagnostic(None, "response", "headers_omitted");
            return;
        }
        let work = async {
            for hook in &self.hooks.hooks {
                if !self.observe_active("response") {
                    break;
                }
                let _ = self
                    .invoke(hook, "response", |c| {
                        hook.callback.after_response(
                            TransportResponseInput {
                                request: &self.info,
                                attempt,
                                status,
                                headers,
                                headers_complete: complete,
                            },
                            c,
                        )
                    })
                    .await;
            }
        };
        if tokio::time::timeout_at(
            (tokio::time::Instant::now() + Duration::from_secs(30)).min(self.deadline),
            work,
        )
        .await
        .is_err()
        {
            self.diagnostic(None, "response", "observation_phase_timed_out");
        }
    }
    pub async fn event(&self, attempt: u32, sequence: u64, data: &Value, redacted: bool) {
        if bound(data, MAX_TRANSPORT_EVENT_BYTES).is_err() {
            self.omitted_event();
            return;
        }
        let work = async {
            for hook in &self.hooks.hooks {
                if !self.observe_active("stream") {
                    break;
                }
                let _ = self
                    .invoke(hook, "stream", |c| {
                        hook.callback.stream_event(
                            TransportStreamInput {
                                request: &self.info,
                                attempt,
                                sequence,
                                data,
                                credentials_redacted: redacted,
                            },
                            c,
                        )
                    })
                    .await;
            }
        };
        if tokio::time::timeout_at(
            (tokio::time::Instant::now() + Duration::from_secs(30)).min(self.deadline),
            work,
        )
        .await
        .is_err()
        {
            self.diagnostic(None, "stream", "observation_phase_timed_out");
        }
    }
    pub fn omitted_unparseable_event(&self) {
        self.diagnostic(None, "stream", "event_unparseable");
    }
    pub fn omitted_event(&self) {
        self.diagnostic(None, "stream", "event_too_large");
    }
}
pub(crate) fn bound(value: &impl Serialize, limit: usize) -> Result<usize> {
    use std::io::Write;
    struct Count {
        n: usize,
        max: usize,
    }
    impl Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if b.len() > self.max.saturating_sub(self.n) {
                return Err(std::io::Error::other("transport data budget exceeded"));
            }
            self.n += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = Count { n: 0, max: limit };
    serde_json::to_writer(&mut w, value)?;
    Ok(w.n)
}
