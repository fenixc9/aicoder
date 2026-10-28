//! Cooperative cancellation shared by agent frontends and in-flight execution stages.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex},
};

use tokio_util::sync::CancellationToken;

const DEFAULT_REASON: &str = "Turn execution was cancelled";

#[derive(Clone, Debug)]
pub struct TurnExecutionContext {
    token: CancellationToken,
    reason: Arc<Mutex<Option<Arc<str>>>>,
    parent: Option<Arc<TurnExecutionContext>>,
}

impl TurnExecutionContext {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            reason: Arc::new(Mutex::new(None)),
            parent: None,
        }
    }

    pub fn cancel(&self, reason: impl Into<Arc<str>>) {
        let mut stored = self
            .reason
            .lock()
            .expect("cancellation reason lock poisoned");
        if stored.is_none() && !self.token.is_cancelled() {
            *stored = Some(reason.into());
        }
        drop(stored);
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn reason(&self) -> Arc<str> {
        self.reason
            .lock()
            .expect("cancellation reason lock poisoned")
            .clone()
            .unwrap_or_else(|| {
                self.parent
                    .as_ref()
                    .map_or_else(|| Arc::from(DEFAULT_REASON), |parent| parent.reason())
            })
    }

    /// Parent cancellation propagates down; stopping a child does not cancel its caller.
    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
            reason: Arc::new(Mutex::new(None)),
            parent: Some(Arc::new(self.clone())),
        }
    }

    pub async fn cancelled(&self) -> TurnCancelled {
        self.token.cancelled().await;
        self.error()
    }

    pub fn error(&self) -> TurnCancelled {
        TurnCancelled {
            reason: self.reason(),
        }
    }
}

impl Default for TurnExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnCancelled {
    reason: Arc<str>,
}

impl TurnCancelled {
    pub(crate) fn new(reason: String) -> Self {
        Self {
            reason: reason.into(),
        }
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for TurnCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl Error for TurnCancelled {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clones_observe_the_first_cancellation_reason() {
        let context = TurnExecutionContext::new();
        let observer = context.clone();

        context.cancel("user requested stop");
        context.cancel("later reason");

        assert!(observer.is_cancelled());
        assert_eq!(observer.cancelled().await.reason(), "user requested stop");
    }

    #[tokio::test]
    async fn child_cleanup_cannot_replace_an_inherited_cancellation_reason() {
        let parent = TurnExecutionContext::new();
        let child = parent.child();
        parent.cancel("caller stop");
        child.cancel("later cleanup");
        assert_eq!(child.reason().as_ref(), "caller stop");
        let parent = TurnExecutionContext::new();
        let child = parent.child();
        child.cancel("child stop");
        assert!(!parent.is_cancelled());
        assert_eq!(child.reason().as_ref(), "child stop");
    }
}
