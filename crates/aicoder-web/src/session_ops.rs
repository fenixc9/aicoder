//! One authenticated host-owned session operation; disconnect is not owner cancellation.
use super::*;
use aicoder_core::{
    TurnExecutionContext,
    session::{ForkPoint, SessionForkLimits, SessionForkPublishFailure},
};
use futures::FutureExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{sync::Notify, task::JoinHandle};
pub(crate) struct SessionJob {
    pub id: String,
    pub request_id: u64,
    pub ui: aicoder_core::ui::UiHost,
    context: TurnExecutionContext,
    result: Mutex<Option<Value>>,
    owner: Mutex<Option<JoinHandle<()>>>,
    changed: Notify,
}
impl SessionJob {
    pub fn active(&self) -> bool {
        self.result.lock().expect("session result").is_none()
    }
    pub fn snapshot(&self) -> Value {
        json!({"id":self.id,"request_id":self.request_id,"ui":self.ui.snapshot(),"result":*self.result.lock().expect("session result")})
    }
    pub fn cancel(&self, reason: &str) {
        self.ui.close();
        self.context.cancel(reason);
    }
    pub async fn join(&self) {
        let owner = self.owner.lock().expect("session owner").take();
        if let Some(owner) = owner {
            let _ = owner.await;
        } else {
            loop {
                let changed = self.changed.notified();
                if !self.active() {
                    break;
                }
                changed.await;
            }
        }
    }
}
impl WebHost {
    pub(crate) fn session_operation(&self) -> Option<Arc<SessionJob>> {
        self.0
            .state
            .lock()
            .expect("Web state")
            .session_operation
            .clone()
    }
    pub(crate) fn start_fork(
        &self,
        id: u64,
        point: ForkPoint,
    ) -> std::result::Result<Arc<SessionJob>, (&'static str, &'static str)> {
        let mut state = self.0.state.lock().expect("Web state");
        if state.closing {
            return Err(("closing", "Host is shutting down"));
        }
        if id == 0 || id > MAX_ID || id <= state.last_id {
            return Err((
                "stale_id",
                "IDs must strictly increase and fit a JavaScript-safe integer",
            ));
        }
        state.last_id = id;
        if state
            .session_operation
            .as_ref()
            .is_some_and(|job| job.active())
            || state.command.as_ref().is_some_and(|c| c.active())
            || state
                .current
                .as_ref()
                .is_some_and(|r| r.completion.lock().expect("Web completion").is_none())
        {
            return Err(("busy", "Wait for the owned operation"));
        }
        let SessionSelection::Existing(source) = &state.selection else {
            return Err(("no_session", "Start or select a session first"));
        };
        let source = source.clone();
        let ui = aicoder_core::ui::UiHost::interactive();
        let context = TurnExecutionContext::new()
            .with_approval_sessions(self.0.approvals.clone())
            .with_ui(ui.client("session").expect("fixed session UI owner"));
        let job = Arc::new(SessionJob {
            id: ui.snapshot().scope.to_string(),
            request_id: id,
            ui,
            context: context.clone(),
            result: Mutex::new(None),
            owner: Mutex::new(None),
            changed: Notify::new(),
        });
        state.session_operation = Some(job.clone());
        let repository = self.0.repository.clone();
        let agent = self.0.agent.clone();
        let weak = Arc::downgrade(&self.0);
        let record = job.clone();
        let owner = tokio::spawn(async move {
            let outcome = std::panic::AssertUnwindSafe(agent.fork_session(
                repository.as_ref(),
                &source,
                point,
                SessionForkLimits::default(),
                context,
            ))
            .catch_unwind()
            .await;
            record.ui.close();
            let result = match outcome {
                Ok(Ok(output)) => {
                    let target = output.session.metadata().clone();
                    if let Some(inner) = weak.upgrade() {
                        let mut state = inner.state.lock().expect("Web state");
                        inner
                            .approvals
                            .remove(&inner.runtime.config().workspace, &source);
                        inner
                            .approvals
                            .remove(&inner.runtime.config().workspace, &target.id);
                        state.selection = SessionSelection::Existing(target.id.clone());
                        state.adopted_session = Some(target.id.clone());
                    }
                    json!({"status":"completed","session":target,"editor":output.editor,"provenance":output.provenance,"session_lifecycle":output.reports})
                }
                Ok(Err(error)) => {
                    if let Some(error) = error.downcast_ref::<SessionForkPublishFailure>() {
                        json!({"status":"publication_uncertain","visible_target":error.visible,"stage":error.stage})
                    } else {
                        json!({"status":"failed","message":aicoder_core::redaction::sanitize_text(&error.to_string(),2048)})
                    }
                }
                Err(_) => {
                    json!({"status":"owner_failed","message":"Session operation ended without a result; inspect stored sessions"})
                }
            };
            *record.result.lock().expect("session result") = Some(result);
            record.changed.notify_waiters();
        });
        *job.owner.lock().expect("session owner") = Some(owner);
        Ok(job)
    }
}
