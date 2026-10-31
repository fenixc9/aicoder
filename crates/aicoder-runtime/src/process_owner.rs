use std::process::Child;
/// Own the unreaped leader until group signalling is complete; no signal uses a recycled PID.
pub(crate) struct ManagedChild {
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
    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }
    pub fn try_reap(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if self.group.is_some() {
            return Err(std::io::Error::other(
                "process group must be signalled before reaping",
            ));
        }
        if !self.exited()? {
            return Ok(None);
        }
        let mut child = self
            .child
            .take()
            .ok_or_else(|| std::io::Error::other("child ownership lost"))?;
        child.wait().map(Some)
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.signal_group();
        if let Some(mut child) = self.child.take() {
            // A dropped tool cannot await. The signal above is synchronous; only reaping
            // happens on this short-lived OS waiter, including during runtime shutdown.
            if let Err(error) = std::thread::Builder::new()
                .name("aicoder-process-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                })
            {
                tracing::warn!(%error,"could not start process reaper");
            }
        }
    }
}
