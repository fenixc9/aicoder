use super::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    sync::{Notify, oneshot},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
/// Maximum cancellation boundaries retained by a derived UI client.
pub const MAX_UI_SCOPE_DEPTH: usize = 64;
type ScopeTokens = Option<Arc<[CancellationToken]>>;
fn scope_cancelled(tokens: &ScopeTokens) -> bool {
    tokens
        .as_ref()
        .is_some_and(|tokens| tokens.iter().any(CancellationToken::is_cancelled))
}
async fn scope_cancellation(tokens: &ScopeTokens) {
    if let Some(tokens) = tokens.as_ref().filter(|tokens| !tokens.is_empty()) {
        futures::future::select_all(tokens.iter().map(|token| Box::pin(token.cancelled()))).await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct UiLimits {
    pub max_questions: usize,
    pub max_notices: usize,
    pub max_statuses: usize,
    pub max_operations: u64,
    pub question_timeout: Duration,
}
impl Default for UiLimits {
    fn default() -> Self {
        Self {
            max_questions: 4,
            max_notices: 32,
            max_statuses: 32,
            max_operations: 4096,
            question_timeout: Duration::from_secs(120),
        }
    }
}
impl UiLimits {
    fn valid(&self) -> bool {
        (1..=16).contains(&self.max_questions)
            && (1..=64).contains(&self.max_notices)
            && (1..=64).contains(&self.max_statuses)
            && (1..=65536).contains(&self.max_operations)
            && (Duration::from_millis(1)..=Duration::from_secs(600))
                .contains(&self.question_timeout)
    }
}
struct Pending {
    question: UiQuestion,
    response: oneshot::Sender<UiAnswer>,
    cancellation: ScopeTokens,
    deadline: Instant,
}
struct State {
    sequence: u64,
    revision: u64,
    questions: BTreeMap<UiRequestId, Pending>,
    notices: VecDeque<UiNotice>,
    omitted: u64,
    statuses: BTreeMap<(String, String), UiStatus>,
}
impl State {
    fn expire(&mut self) {
        let before = self.questions.len();
        self.questions.retain(|_, p| {
            !p.response.is_closed()
                && !scope_cancelled(&p.cancellation)
                && p.deadline > Instant::now()
        });
        if before != self.questions.len() {
            self.revision += 1;
        }
    }
    fn next(&mut self, limit: u64) -> Result<u64, UiError> {
        if self.sequence >= limit {
            return Err(UiError::Full);
        }
        self.sequence += 1;
        self.revision += 1;
        Ok(self.sequence)
    }
}
struct Shared {
    scope: Uuid,
    capabilities: UiCapabilities,
    limits: UiLimits,
    state: Mutex<State>,
    closed: CancellationToken,
    changed: Notify,
}
struct Owner(Arc<Shared>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.close();
    }
}
impl Shared {
    fn close(&self) {
        self.closed.cancel();
        let mut state = self.state.lock().expect("UI state");
        state.questions.clear();
        state.statuses.clear();
        state.revision = state.revision.saturating_add(1);
        self.changed.notify_waiters();
    }
}
/// Cloneable frontend owner. Dropping its last clone expires all questions synchronously.
#[derive(Clone)]
pub struct UiHost(Arc<Owner>);
impl UiHost {
    pub fn new(capabilities: UiCapabilities, limits: UiLimits) -> Result<Self, UiError> {
        if !limits.valid() {
            return Err(UiError::Invalid);
        }
        Ok(Self(Arc::new(Owner(Arc::new(Shared {
            scope: Uuid::new_v4(),
            capabilities,
            limits,
            state: Mutex::new(State {
                sequence: 0,
                revision: 0,
                questions: BTreeMap::new(),
                notices: VecDeque::new(),
                omitted: 0,
                statuses: BTreeMap::new(),
            }),
            closed: CancellationToken::new(),
            changed: Notify::new(),
        })))))
    }
    pub fn interactive() -> Self {
        Self::new(UiCapabilities::interactive(), UiLimits::default()).expect("valid UI defaults")
    }
    pub fn client(&self, owner: &str) -> Result<UiClient, UiError> {
        if crate::extensions::name(owner).is_err() {
            return Err(UiError::Invalid);
        }
        Ok(UiClient {
            shared: Some(Arc::downgrade(&self.0.0)),
            owner: owner.into(),
            cancellation: None,
            scope_exhausted: false,
        })
    }
    pub fn close(&self) {
        self.0.0.close();
    }
    pub fn snapshot(&self) -> UiSnapshot {
        let shared = &self.0.0;
        let mut state = shared.state.lock().expect("UI state");
        state.expire();
        UiSnapshot {
            version: 1,
            revision: state.revision,
            scope: shared.scope,
            closed: shared.closed.is_cancelled(),
            capabilities: shared.capabilities,
            questions: state
                .questions
                .values()
                .map(|p| p.question.clone())
                .collect(),
            notices: state.notices.iter().cloned().collect(),
            omitted_notices: state.omitted,
            statuses: state.statuses.values().cloned().collect(),
        }
    }
    /// Wait for a revision change, including deadlines and cancellation, without losing wakeups.
    pub async fn wait(&self, after: u64) -> UiSnapshot {
        loop {
            let changed = self.0.0.changed.notified();
            let snapshot = self.snapshot();
            if snapshot.revision != after || snapshot.closed {
                return snapshot;
            }
            tokio::select! { _ = changed => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
    }
    pub fn answer(&self, id: UiRequestId, answer: UiAnswer) -> Result<(), UiError> {
        let shared = &self.0.0;
        if shared.closed.is_cancelled() {
            return Err(UiError::Closed);
        }
        let mut state = shared.state.lock().expect("UI state");
        state.expire();
        let pending = state.questions.get(&id).ok_or(UiError::Stale)?;
        if !pending.question.prompt.accepts(&answer) {
            return Err(UiError::Invalid);
        }
        let pending = state.questions.remove(&id).expect("checked question");
        state.revision += 1;
        pending.response.send(answer).map_err(|_| UiError::Stale)?;
        shared.changed.notify_waiters();
        Ok(())
    }
}
/// Weak host capability; default/headless clients fail immediately without reading stdin.
#[derive(Clone, Default)]
pub struct UiClient {
    shared: Option<Weak<Shared>>,
    owner: String,
    cancellation: ScopeTokens,
    scope_exhausted: bool,
}
impl UiClient {
    pub(crate) fn callback_cancellation(&self) -> CancellationToken {
        self.cancellation
            .as_ref()
            .and_then(|tokens| tokens.last())
            .map(CancellationToken::child_token)
            .unwrap_or_default()
    }
    pub(crate) fn scoped(&self, cancellation: CancellationToken) -> Self {
        if self.shared.is_none() {
            return self.clone();
        }
        let inherited = self.cancellation.as_deref().unwrap_or_default();
        if self.scope_exhausted || inherited.len() >= MAX_UI_SCOPE_DEPTH {
            return Self {
                scope_exhausted: true,
                ..self.clone()
            };
        }
        let mut tokens = inherited.to_vec();
        tokens.push(cancellation);
        Self {
            cancellation: Some(tokens.into()),
            ..self.clone()
        }
    }

    pub(crate) fn attached(&self) -> bool {
        self.shared.is_some()
    }
    pub(crate) fn named(&self, owner: &str) -> Self {
        if self.shared.is_none() {
            return self.clone();
        }
        let mut client = self.clone();
        client.owner = owner
            .bytes()
            .filter(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
            .take(64)
            .map(char::from)
            .collect();
        if client.owner.is_empty() {
            client.owner = "extension".into();
        }
        client
    }
    fn shared(&self) -> Result<Arc<Shared>, UiError> {
        if scope_cancelled(&self.cancellation) {
            return Err(UiError::Cancelled);
        }
        if self.scope_exhausted {
            return Err(UiError::Full);
        }
        let shared = self
            .shared
            .as_ref()
            .ok_or(UiError::Unsupported)?
            .upgrade()
            .ok_or(UiError::Closed)?;
        if shared.closed.is_cancelled() {
            return Err(UiError::Closed);
        }
        Ok(shared)
    }
    pub fn capabilities(&self) -> UiCapabilities {
        self.shared().map(|s| s.capabilities).unwrap_or_default()
    }
    pub fn notify(&self, level: UiLevel, text: String) -> Result<u64, UiError> {
        let shared = self.shared()?;
        if !shared.capabilities.notifications {
            return Err(UiError::Unsupported);
        }
        if !super::protocol::text(&text, 2048) {
            return Err(UiError::Invalid);
        }
        let mut state = shared.state.lock().expect("UI state");
        if shared.closed.is_cancelled() || scope_cancelled(&self.cancellation) {
            return Err(UiError::Closed);
        }
        let sequence = state.next(shared.limits.max_operations)?;
        if state.notices.len() >= shared.limits.max_notices {
            state.notices.pop_front();
            state.omitted = state.omitted.saturating_add(1);
        }
        state.notices.push_back(UiNotice {
            sequence,
            owner: self.owner.clone(),
            level,
            text,
        });
        shared.changed.notify_waiters();
        Ok(sequence)
    }
    pub fn status(&self, key: &str, text: Option<String>) -> Result<(), UiError> {
        let shared = self.shared()?;
        if !shared.capabilities.status {
            return Err(UiError::Unsupported);
        }
        if crate::extensions::name(key).is_err()
            || text
                .as_ref()
                .is_some_and(|s| !super::protocol::text(s, 1024))
        {
            return Err(UiError::Invalid);
        }
        let mut state = shared.state.lock().expect("UI state");
        if shared.closed.is_cancelled() || scope_cancelled(&self.cancellation) {
            return Err(UiError::Closed);
        }
        let identity = (self.owner.clone(), key.to_string());
        if text.is_some()
            && state.statuses.len() >= shared.limits.max_statuses
            && !state.statuses.contains_key(&identity)
        {
            return Err(UiError::Full);
        }
        state.next(shared.limits.max_operations)?;
        if let Some(text) = text {
            state.statuses.insert(
                identity,
                UiStatus {
                    owner: self.owner.clone(),
                    key: key.into(),
                    text,
                },
            );
        } else {
            state.statuses.remove(&identity);
        }
        shared.changed.notify_waiters();
        Ok(())
    }
    pub async fn ask(&self, prompt: UiPrompt) -> Result<UiAnswer, UiError> {
        let shared = self.shared()?;
        let supported = match &prompt {
            UiPrompt::Select { .. } => shared.capabilities.select,
            UiPrompt::Input { .. } => shared.capabilities.input,
        };
        if !supported {
            return Err(UiError::Unsupported);
        }
        if !prompt.valid() {
            return Err(UiError::Invalid);
        }
        let (send, receive) = oneshot::channel();
        let deadline = Instant::now() + shared.limits.question_timeout;
        let id = {
            let mut state = shared.state.lock().expect("UI state");
            state.expire();
            if shared.closed.is_cancelled() || scope_cancelled(&self.cancellation) {
                return Err(UiError::Closed);
            }
            if state.questions.len() >= shared.limits.max_questions {
                return Err(UiError::Full);
            }
            let id = UiRequestId {
                scope: shared.scope,
                sequence: state.next(shared.limits.max_operations)?,
            };
            state.questions.insert(
                id,
                Pending {
                    question: UiQuestion {
                        id,
                        owner: self.owner.clone(),
                        prompt,
                    },
                    response: send,
                    cancellation: self.cancellation.clone(),
                    deadline,
                },
            );
            id
        };
        let _guard = Question {
            shared: shared.clone(),
            id,
        };
        shared.changed.notify_waiters();
        tokio::select! {biased;
            _=scope_cancellation(&self.cancellation)=>Err(UiError::Cancelled),
            _=shared.closed.cancelled()=>Err(UiError::Closed),
            _=tokio::time::sleep_until(deadline)=>Err(UiError::TimedOut),
            answer=receive=>answer.map_err(|_|UiError::Closed),
        }
    }
}
struct Question {
    shared: Arc<Shared>,
    id: UiRequestId,
}
impl Drop for Question {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().expect("UI state");
        if state.questions.remove(&self.id).is_some() {
            state.revision += 1;
        }
        self.shared.changed.notify_waiters();
    }
}
impl std::fmt::Debug for UiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiClient")
            .field("capabilities", &self.capabilities())
            .finish_non_exhaustive()
    }
}
