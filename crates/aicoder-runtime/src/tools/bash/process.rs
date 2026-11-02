use std::{os::unix::process::ExitStatusExt, time::Duration};
use tokio::signal::unix::Signal;
const CLEANUP_TIME: Duration = Duration::from_millis(250);
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Stop {
    Exit,
    Timeout,
    ToolTimeout,
    Cancelled,
    OutputLimit,
    CaptureError,
    WaitError,
}
impl Stop {
    pub fn name(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Timeout => "timeout",
            Self::ToolTimeout => "tool_timeout",
            Self::Cancelled => "cancelled",
            Self::OutputLimit => "output_limit",
            Self::CaptureError => "capture_error",
            Self::WaitError => "wait_error",
        }
    }
}
pub(super) struct Completion {
    pub stop: Stop,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub signal_requested: bool,
    pub reaped: bool,
    pub cleanup_ok: bool,
}
impl Completion {
    pub fn tool_timeout(spawned: bool) -> Self {
        Self {
            stop: Stop::ToolTimeout,
            exit_code: None,
            signal: None,
            signal_requested: spawned,
            reaped: false,
            cleanup_ok: !spawned,
        }
    }
}
pub(super) use crate::process_owner::ManagedChild;
impl ManagedChild {
    pub(super) async fn reap(
        &mut self,
        exits: &mut Signal,
        stop: Stop,
        signal_ok: bool,
    ) -> Completion {
        if !self.has_child() {
            return Completion {
                stop: Stop::WaitError,
                exit_code: None,
                signal: None,
                signal_requested: false,
                reaped: false,
                cleanup_ok: false,
            };
        }
        let wait = async {
            loop {
                if let Some(status) = self.try_reap()? {
                    return Ok(status);
                }
                if exits.recv().await.is_none() {
                    return Err(std::io::Error::other("child signal stream closed"));
                }
            }
        };
        match tokio::time::timeout(CLEANUP_TIME, wait).await {
            Ok(Ok(status)) => Completion {
                stop,
                exit_code: status.code(),
                signal: status.signal(),
                signal_requested: true,
                reaped: true,
                cleanup_ok: signal_ok,
            },
            result => Completion {
                stop: if matches!(result, Ok(Err(_))) {
                    Stop::WaitError
                } else {
                    stop
                },
                exit_code: None,
                signal: None,
                signal_requested: true,
                reaped: false,
                cleanup_ok: false,
            },
        }
    }
}
