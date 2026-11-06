//! Host-selected session lifecycle. Callback data never grants storage or run authority.
use super::*;
use crate::{TurnExecutionContext, session::*};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SessionOperation {
    Navigation { action: ArchiveNavigation },
    New,
    Resume { session_id: String },
    Shutdown { reason: SessionShutdownReason },
    Reload,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionShutdownReason {
    Quit,
    Reload,
    New,
    Resume,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPosition {
    pub id: String,
    pub revision: u64,
    pub leaf_id: Option<String>,
    pub title: Option<String>,
}
impl From<&SessionMetadata> for SessionPosition {
    fn from(m: &SessionMetadata) -> Self {
        Self {
            id: m.id.clone(),
            revision: m.revision,
            leaf_id: m.leaf_id.clone(),
            title: m.title.clone(),
        }
    }
}
pub struct SessionHookInput<'a> {
    pub workspace: &'a Path,
    pub operation: &'a SessionOperation,
    pub source: Option<&'a SessionPosition>,
    pub target: Option<&'a SessionPosition>,
}
#[derive(Debug)]
pub enum SessionDecision {
    Continue,
    Deny { reason: String },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    Started,
    Shutdown,
    Navigated,
    InfoChanged,
    Reloaded,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycleReason {
    Startup,
    New,
    Resume,
    Reload,
    Quit,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLifecycleEvent {
    pub reason: Option<SessionLifecycleReason>,
    pub kind: SessionEventKind,
    pub operation: SessionOperation,
    pub source: Option<SessionPosition>,
    pub target: Option<SessionPosition>,
}
#[async_trait]
pub trait SessionHook: Send + Sync {
    async fn before(
        &self,
        _input: SessionHookInput<'_>,
        _cancellation: ExtensionCancellation,
    ) -> anyhow::Result<SessionDecision> {
        Ok(SessionDecision::Continue)
    }
    /// Observation after adoption. Failure cannot veto or retry a completed change.
    async fn after(
        &self,
        _event: &SessionLifecycleEvent,
        _cancellation: ExtensionCancellation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionObservationFailure {
    pub extension: String,
    pub hook: String,
    pub class: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionLifecycleDelivery {
    pub registered: usize,
    pub invoked: usize,
    pub completed: usize,
    pub failures: Vec<SessionObservationFailure>,
}
#[derive(Debug, Clone)]
pub struct SessionActionDenied {
    pub extension: String,
    pub hook: String,
    pub reason: String,
}
impl std::fmt::Display for SessionActionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session change denied by {}/{}: {}",
            self.extension, self.hook, self.reason
        )
    }
}
impl std::error::Error for SessionActionDenied {}
#[derive(Clone)]
pub(crate) struct SessionHooks {
    hooks: Vec<Registered<dyn SessionHook>>,
}
impl SessionHooks {
    pub fn new(hooks: Vec<Registered<dyn SessionHook>>) -> Self {
        Self { hooks }
    }
    pub async fn before(
        &self,
        workspace: &Path,
        operation: &SessionOperation,
        source: Option<&SessionPosition>,
        target: Option<&SessionPosition>,
        context: &TurnExecutionContext,
    ) -> anyhow::Result<()> {
        size(&(operation, source, target), 64 * 1024)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let work = async {
            for h in &self.hooks {
                let started = tokio::time::Instant::now();
                let vote = invoke(&h.owner, &h.spec, context.ui(), |c| {
                    h.callback.before(
                        SessionHookInput {
                            workspace,
                            operation,
                            source,
                            target,
                        },
                        c,
                    )
                })
                .await?;
                anyhow::ensure!(
                    started.elapsed() <= h.spec.timeout && tokio::time::Instant::now() < deadline,
                    "session guard deadline expired"
                );
                if let SessionDecision::Deny { reason } = vote {
                    size(&reason, 4096)?;
                    return Err(SessionActionDenied {
                        extension: h.owner.clone(),
                        hook: h.spec.id.clone(),
                        reason: crate::redaction::sanitize_text(&reason, 4096),
                    }
                    .into());
                }
            }
            Ok(())
        };
        tokio::select! {biased;cancelled=context.cancelled()=>Err(cancelled.into()),result=tokio::time::timeout(Duration::from_secs(30),work)=>result.map_err(|_|anyhow::anyhow!("session guard phase timed out"))?}
    }
    pub async fn after(
        &self,
        event: &SessionLifecycleEvent,
        context: &TurnExecutionContext,
        run_deadline: Option<tokio::time::Instant>,
    ) -> SessionLifecycleDelivery {
        let mut delivery = SessionLifecycleDelivery {
            registered: self.hooks.len(),
            ..Default::default()
        };
        let phase_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let deadline = run_deadline.map_or(phase_deadline, |d| d.min(phase_deadline));
        for h in &self.hooks {
            let mut invoked = false;
            let result = if tokio::time::Instant::now() >= deadline {
                Err("phase_timeout")
            } else if size(event, 64 * 1024).is_err() {
                Err("event_limit")
            } else {
                let started = tokio::time::Instant::now();
                let work = async {
                    invoked = true;
                    invoke(&h.owner, &h.spec, context.ui(), |c| {
                        h.callback.after(event, c)
                    })
                    .await
                };
                let result = tokio::select! {biased;
                    _=context.cancelled()=>Err("cancelled"),
                    result=tokio::time::timeout_at(deadline,work)=>match result{Ok(Ok(()))=>Ok(()),Ok(Err(_))=>Err("callback_failed"),Err(_)=>Err("phase_timeout")}
                };
                if started.elapsed() > h.spec.timeout {
                    Err("callback_timeout")
                } else {
                    result
                }
            };
            delivery.invoked += usize::from(invoked);
            match result {
                Ok(()) => delivery.completed += 1,
                Err(class) => delivery.failures.push(SessionObservationFailure {
                    extension: h.owner.clone(),
                    hook: h.spec.id.clone(),
                    class: class.into(),
                }),
            }
        }
        delivery
    }
}
