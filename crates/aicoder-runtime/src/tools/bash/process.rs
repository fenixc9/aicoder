use std::{os::unix::process::ExitStatusExt, process::Child, time::Duration};
use tokio::signal::unix::Signal;
const CLEANUP_TIME: Duration = Duration::from_millis(250);
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Stop {
    Exit,
    Timeout,
    ToolTimeout,
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
/// Own the unreaped leader until group signalling is complete; no signal uses a recycled PID.
pub(super) struct ManagedChild {
    child: Option<Child>,
    group: Option<i32>,
}
impl ManagedChild {
    pub fn new(child: Child) -> Self {
        let group = i32::try_from(child.id()).expect("Unix PID");
        Self {
            child: Some(child),
            group: Some(group),
        }
    }
    pub fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("owned child")
    }
    pub fn exited(&mut self) -> std::io::Result<bool> {
        loop {
            let Some(child) = self.child.as_ref() else {
                return Ok(true);
            };
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // WNOWAIT observes exit while retaining leader ownership until group signalling.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(info.si_signo != 0);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                // A host reaped/auto-reaped our child. Disown it instead of signalling
                // an ID which may now belong to an unrelated process.
                self.group = None;
                self.child = None;
            }
            return Err(error);
        }
    }
    pub fn signal_group(&mut self) -> bool {
        let Some(group) = self.group.take() else {
            return true;
        };
        let result = unsafe { libc::kill(-group, libc::SIGKILL) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    pub async fn reap(&mut self, exits: &mut Signal, stop: Stop, signal_ok: bool) -> Completion {
        if self.child.is_none() {
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
                if self.exited()? {
                    let mut child = self.child.take().expect("owned exited child");
                    return child.wait();
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
impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.signal_group();
        if let Some(mut child) = self.child.take() {
            // A dropped tool cannot await. The signal above is synchronous; only reaping
            // happens on this short-lived OS waiter, including during runtime shutdown.
            if let Err(error) = std::thread::Builder::new()
                .name("aicoder-shell-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                })
            {
                tracing::warn!(%error,"could not start shell reaper");
            }
        }
    }
}
