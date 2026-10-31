use super::*;
use crate::process_owner::ManagedChild;
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::MetadataExt,
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::Instant,
};
use tokio::sync::{Notify, oneshot};
const QUEUED: u8 = 0;
const ACTIVE: u8 = 1;
const COMPLETED: u8 = 2;
const ABANDONED: u8 = 3;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Initialize,
    Prepare,
    Execute,
    Dispose,
}
pub(super) struct Shared {
    process: Mutex<Option<ManagedChild>>,
    report: Mutex<ProcessDiagnostics>,
    closing: AtomicBool,
    done: Notify,
    pub workspace: PathBuf,
    pub stamp: (u64, u64),
    pub limits: ProcessLimits,
    pub plans: AtomicUsize,
    wake: UnixStream,
}
impl Shared {
    fn wake(&self) {
        let byte = 1u8;
        // Wakeups must not change the embedding host's SIGPIPE policy.
        unsafe {
            libc::send(
                self.wake.as_raw_fd(),
                (&byte as *const u8).cast(),
                1,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            );
        }
    }
    pub fn diagnostics(&self) -> ProcessDiagnostics {
        self.report.lock().expect("process diagnostics").clone()
    }
    pub fn error(&self, kind: ProcessErrorKind, executed: bool) -> ProcessError {
        ProcessError {
            kind,
            may_have_executed: executed,
            remote_code: None,
            diagnostics: self.diagnostics(),
        }
    }
    pub fn close(&self, reason: ProcessErrorKind) {
        self.closing.store(true, Ordering::SeqCst);
        {
            let mut report = self.report.lock().expect("process diagnostics");
            report.closed = true;
            if report.pid.is_some() && !report.reaped {
                report.cleanup_pending = true;
            }
            report.stop.get_or_insert(reason);
        }
        if let Some(process) = self.process.lock().expect("process owner").as_mut() {
            let _ = process.exited(); // Disown a leader already reaped by a foreign host handler.
            process.signal_group();
        }
        self.wake();
    }
    pub fn workspace_matches(&self, path: &std::path::Path) -> bool {
        path == self.workspace
            && std::fs::metadata(path).is_ok_and(|m| (m.dev(), m.ino()) == self.stamp && m.is_dir())
    }
    async fn stopped(&self) {
        loop {
            let ready = self.done.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if self.diagnostics().worker_done {
                break;
            }
            ready.await;
        }
    }
}
struct Work {
    id: u64,
    frame: Vec<u8>,
    class: Class,
    deadline: Instant,
    state: Arc<AtomicU8>,
    reply: Option<oneshot::Sender<Result<Value, ProcessError>>>,
}
pub(super) struct Client {
    tx: mpsc::SyncSender<Work>,
    pub shared: Arc<Shared>,
    next: AtomicU64,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.shared.close(ProcessErrorKind::Shutdown);
    }
}
struct RequestGuard {
    shared: Arc<Shared>,
    state: Arc<AtomicU8>,
    class: Class,
    armed: bool,
}
impl RequestGuard {
    fn abort(&mut self, reason: ProcessErrorKind) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if self
            .state
            .compare_exchange(QUEUED, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
        if self
            .state
            .compare_exchange(ACTIVE, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            || (self.state.load(Ordering::SeqCst) == COMPLETED && self.class == Class::Prepare)
        {
            self.shared.close(reason);
        }
    }
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.abort(ProcessErrorKind::Cancelled);
    }
}
struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("frame limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn fits(value: &impl Serialize, limit: usize) -> bool {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("payload limit"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count { bytes: 0, limit }, value).is_ok()
}
impl Client {
    pub fn start(mut config: ProcessConfig) -> Result<Arc<Self>, ProcessError> {
        let invalid = || ProcessError {
            kind: ProcessErrorKind::InvalidConfiguration,
            may_have_executed: false,
            remote_code: None,
            diagnostics: Default::default(),
        };
        if !config.valid() {
            return Err(invalid());
        }
        config.workspace = config.workspace.canonicalize().map_err(|_| invalid())?;
        let metadata = std::fs::metadata(&config.workspace).map_err(|_| invalid())?;
        if !metadata.is_dir() {
            return Err(invalid());
        }
        let (wake, wake_reader) = UnixStream::pair().map_err(|_| invalid())?;
        nonblocking(&wake).map_err(|_| invalid())?;
        nonblocking(&wake_reader).map_err(|_| invalid())?;
        let shared = Arc::new(Shared {
            process: Mutex::new(None),
            report: Mutex::new(Default::default()),
            closing: AtomicBool::new(false),
            done: Notify::new(),
            workspace: config.workspace.clone(),
            stamp: (metadata.dev(), metadata.ino()),
            limits: config.limits,
            plans: AtomicUsize::new(0),
            wake,
        });
        let (tx, rx) = mpsc::sync_channel(config.limits.max_queued);
        let owner = shared.clone();
        std::thread::Builder::new()
            .name("aicoder-tool-process".into())
            .spawn(move || worker(config, rx, owner, wake_reader))
            .map_err(|_| shared.error(ProcessErrorKind::StartFailed, false))?;
        Ok(Arc::new(Self {
            tx,
            shared,
            next: AtomicU64::new(1),
        }))
    }
    fn work(
        &self,
        method: &str,
        params: impl Serialize,
        class: Class,
        duration: Duration,
        reply: Option<oneshot::Sender<Result<Value, ProcessError>>>,
    ) -> Result<Work, ProcessError> {
        if self.shared.diagnostics().closed {
            return Err(self.shared.error(ProcessErrorKind::Closed, false));
        }
        let id = self
            .next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
            .map_err(|_| self.shared.error(ProcessErrorKind::Closed, false))?;
        #[derive(Serialize)]
        struct Request<'a, T> {
            jsonrpc: &'static str,
            id: u64,
            method: &'a str,
            params: T,
        }
        let mut writer = Limited {
            bytes: Vec::new(),
            limit: self.shared.limits.max_request_bytes.saturating_sub(1),
        };
        serde_json::to_writer(
            &mut writer,
            &Request {
                jsonrpc: "2.0",
                id,
                method,
                params,
            },
        )
        .map_err(|_| self.shared.error(ProcessErrorKind::InputLimit, false))?;
        writer.bytes.push(b'\n');
        let deadline = Instant::now().checked_add(duration).ok_or_else(|| {
            self.shared
                .error(ProcessErrorKind::InvalidConfiguration, false)
        })?;
        Ok(Work {
            id,
            frame: writer.bytes,
            class,
            deadline,
            state: Arc::new(AtomicU8::new(QUEUED)),
            reply,
        })
    }
    pub async fn call(
        &self,
        method: &str,
        params: impl Serialize,
        class: Class,
        duration: Duration,
    ) -> Result<Value, ProcessError> {
        let (tx, rx) = oneshot::channel();
        let work = self.work(method, params, class, duration, Some(tx))?;
        let state = work.state.clone();
        self.tx.try_send(work).map_err(|error| {
            self.shared.error(
                if matches!(error, mpsc::TrySendError::Full(_)) {
                    ProcessErrorKind::Busy
                } else {
                    ProcessErrorKind::Closed
                },
                false,
            )
        })?;
        self.shared.wake();
        let mut guard = RequestGuard {
            shared: self.shared.clone(),
            state: state.clone(),
            class,
            armed: true,
        };
        match tokio::time::timeout(duration, rx).await {
            Ok(Ok(result)) => {
                guard.armed = false;
                result
            }
            result => {
                let kind = if result.is_err() {
                    ProcessErrorKind::Timeout
                } else {
                    ProcessErrorKind::Closed
                };
                let executed = class == Class::Execute && state.load(Ordering::SeqCst) != QUEUED;
                guard.abort(kind);
                if self.shared.closing.load(Ordering::SeqCst) {
                    let _ = tokio::time::timeout(
                        self.shared.limits.shutdown_timeout,
                        self.shared.stopped(),
                    )
                    .await;
                }
                Err(self.shared.error(kind, executed))
            }
        }
    }
    pub fn dispose(&self, params: impl Serialize) {
        if self.shared.closing.load(Ordering::SeqCst) {
            return;
        }
        let work = self.work(
            "tools/dispose",
            params,
            Class::Dispose,
            self.shared.limits.request_timeout,
            None,
        );
        if work.is_err() || self.tx.try_send(work.expect("validated disposal")).is_err() {
            self.shared.close(ProcessErrorKind::DisposeFailed);
        } else {
            self.shared.wake();
        }
    }
    pub async fn shutdown(&self) -> ProcessDiagnostics {
        self.shared.close(ProcessErrorKind::Shutdown);
        let _ =
            tokio::time::timeout(self.shared.limits.shutdown_timeout, self.shared.stopped()).await;
        self.shared.diagnostics()
    }
}
fn nonblocking(fd: &impl AsRawFd) -> std::io::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
struct Pipes {
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
}
fn launch(config: &ProcessConfig, shared: &Shared) -> std::io::Result<Pipes> {
    let mut command = Command::new(&config.program);
    command
        .args(&config.arguments)
        .current_dir(&config.workspace)
        .env_clear()
        .envs(&config.environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut owned = ManagedChild::new(command.spawn()?);
    let pipes = Pipes {
        stdin: owned.child_mut().stdin.take().expect("piped stdin"),
        stdout: owned.child_mut().stdout.take().expect("piped stdout"),
        stderr: owned.child_mut().stderr.take().expect("piped stderr"),
    };
    shared.report.lock().expect("process diagnostics").pid = Some(owned.child_mut().id());
    *shared.process.lock().expect("process owner") = Some(owned);
    nonblocking(&pipes.stdin)?;
    nonblocking(&pipes.stdout)?;
    nonblocking(&pipes.stderr)?;
    Ok(pipes)
}
fn worker(
    config: ProcessConfig,
    rx: mpsc::Receiver<Work>,
    shared: Arc<Shared>,
    mut wake: UnixStream,
) {
    let result = if shared.closing.load(Ordering::SeqCst) {
        Err(ProcessErrorKind::Shutdown)
    } else {
        launch(&config, &shared).map_err(|_| ProcessErrorKind::StartFailed)
    };
    drop(config); // Do not retain argv/environment values through the connection lifetime.
    let mut active = None;
    let reason = match result {
        Ok(mut pipes) => {
            // Only this IO worker blocks SIGPIPE; no host signal policy or child mask is changed.
            unsafe {
                let mut mask = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigaddset(&mut mask, libc::SIGPIPE);
                libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            }
            drive(&mut pipes, &rx, &shared, &mut active, &mut wake)
        }
        Err(reason) => reason,
    };
    shared.close(reason);
    cleanup(&shared);
    if let Some((work, sent)) = active {
        finish_error(work, &shared, reason, sent > 0);
    }
    while let Ok(work) = rx.try_recv() {
        finish_error(work, &shared, ProcessErrorKind::Closed, false);
    }
    shared
        .report
        .lock()
        .expect("process diagnostics")
        .worker_done = true;
    shared.done.notify_waiters();
}
fn finish_error(mut work: Work, shared: &Shared, reason: ProcessErrorKind, sent: bool) {
    work.state.store(COMPLETED, Ordering::SeqCst);
    if let Some(reply) = work.reply.take() {
        let _ = reply.send(Err(
            shared.error(reason, work.class == Class::Execute && sent)
        ));
    }
}
fn drive(
    pipes: &mut Pipes,
    rx: &mpsc::Receiver<Work>,
    shared: &Shared,
    active: &mut Option<(Work, usize)>,
    wake: &mut UnixStream,
) -> ProcessErrorKind {
    let mut line = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut exiting = None;
    loop {
        if shared.closing.load(Ordering::SeqCst) {
            return shared
                .diagnostics()
                .stop
                .unwrap_or(ProcessErrorKind::Closed);
        }
        if active.is_none() && exiting.is_none() {
            for _ in 0..=shared.limits.max_queued {
                match rx.try_recv() {
                    Ok(work) => {
                        if work
                            .state
                            .compare_exchange(QUEUED, ACTIVE, Ordering::SeqCst, Ordering::SeqCst)
                            .is_err()
                        {
                            continue;
                        }
                        if Instant::now() >= work.deadline {
                            let dispose = work.class == Class::Dispose;
                            finish_error(work, shared, ProcessErrorKind::Timeout, false);
                            if dispose {
                                return ProcessErrorKind::DisposeFailed;
                            }
                        } else {
                            *active = Some((work, 0));
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return ProcessErrorKind::Shutdown,
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if let Some((work, sent)) = active.as_mut() {
            if Instant::now() >= work.deadline {
                return ProcessErrorKind::Timeout;
            }
            if *sent < work.frame.len() && exiting.is_none() {
                match pipes.stdin.write(&work.frame[*sent..]) {
                    Ok(0) => return ProcessErrorKind::Io,
                    Ok(n) => {
                        *sent += n;
                        if *sent == work.frame.len() {
                            shared
                                .report
                                .lock()
                                .expect("process diagnostics")
                                .requests_sent += 1;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return ProcessErrorKind::Io,
                }
            }
        }
        let mut eof = false;
        for _ in 0..8 {
            match pipes.stdout.read(&mut chunk) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    {
                        let mut r = shared.report.lock().expect("process diagnostics");
                        r.stdout_bytes = r.stdout_bytes.saturating_add(n as u64);
                    }
                    for b in &chunk[..n] {
                        if active.is_none() {
                            return ProcessErrorKind::Protocol;
                        }
                        if *b == b'\n' {
                            if let Err(reason) = response(&line, shared, active) {
                                return reason;
                            }
                            line.clear();
                        } else {
                            if line.len() >= shared.limits.max_frame_bytes.saturating_sub(1) {
                                return ProcessErrorKind::FrameLimit;
                            }
                            line.push(*b);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return ProcessErrorKind::Io,
            }
        }
        for _ in 0..8 {
            match pipes.stderr.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut r = shared.report.lock().expect("process diagnostics");
                    r.stderr_bytes = r.stderr_bytes.saturating_add(n as u64);
                    if r.stderr_bytes > shared.limits.max_stderr_bytes {
                        return ProcessErrorKind::StderrLimit;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return ProcessErrorKind::Io,
            }
        }
        let exited = shared
            .process
            .lock()
            .expect("process owner")
            .as_mut()
            .map(|p| p.exited());
        if eof {
            return ProcessErrorKind::Exited;
        }
        if matches!(exited, Some(Ok(true))) && exiting.is_none() {
            if let Some(process) = shared.process.lock().expect("process owner").as_mut() {
                process.signal_group();
            }
            {
                let mut report = shared.report.lock().expect("process diagnostics");
                report.closed = true;
                report.stop.get_or_insert(ProcessErrorKind::Exited);
            }
            exiting = Some(Instant::now() + Duration::from_millis(250));
        }
        if !matches!(exited, Some(Ok(false)) | Some(Ok(true))) {
            return ProcessErrorKind::Io;
        }
        if exiting.is_some_and(|deadline| Instant::now() >= deadline) {
            return ProcessErrorKind::Exited;
        }
        let mut pause = Duration::from_millis(100);
        if let Some((work, _)) = active.as_ref() {
            pause = pause.min(work.deadline.saturating_duration_since(Instant::now()));
        }
        if let Some(deadline) = exiting {
            pause = pause.min(deadline.saturating_duration_since(Instant::now()));
        }
        let writing = active
            .as_ref()
            .is_some_and(|(work, sent)| *sent < work.frame.len())
            && exiting.is_none();
        let mut fds = [
            libc::pollfd {
                fd: pipes.stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: pipes.stderr.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: pipes.stdin.as_raw_fd(),
                events: if writing { libc::POLLOUT } else { 0 },
                revents: 0,
            },
        ];
        if unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                pause.as_millis().clamp(1, 100) as i32,
            )
        } < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return ProcessErrorKind::Io;
        }
        loop {
            match wake.read(&mut chunk) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    }
}
fn response(
    bytes: &[u8],
    shared: &Shared,
    active: &mut Option<(Work, usize)>,
) -> Result<(), ProcessErrorKind> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err(ProcessErrorKind::Cancelled);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ProcessErrorKind::Protocol)?;
    let object = value.as_object().ok_or(ProcessErrorKind::Protocol)?;
    let Some((work, sent)) = active.as_ref() else {
        return Err(ProcessErrorKind::Protocol);
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_u64) != Some(work.id)
        || *sent != work.frame.len()
        || object.contains_key("method")
        || object.contains_key("result") == object.contains_key("error")
    {
        return Err(ProcessErrorKind::Protocol);
    }
    let result = if let Some(error) = object.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or(ProcessErrorKind::Protocol)?;
        if error.get("message").and_then(Value::as_str).is_none() {
            return Err(ProcessErrorKind::Protocol);
        }
        let mut e = shared.error(ProcessErrorKind::Remote, work.class == Class::Execute);
        e.remote_code = Some(code);
        Err(e)
    } else {
        Ok(object.get("result").expect("exclusive result").clone())
    };
    if work.class == Class::Dispose
        && !result
            .as_ref()
            .is_ok_and(|value| value.get("disposed") == Some(&Value::Bool(true)))
    {
        return Err(ProcessErrorKind::DisposeFailed);
    }
    let (mut work, _) = active.take().expect("matched response");
    if work
        .state
        .compare_exchange(ACTIVE, COMPLETED, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(ProcessErrorKind::Cancelled);
    }
    shared
        .report
        .lock()
        .expect("process diagnostics")
        .completed_requests += 1;
    if let Some(reply) = work.reply.take() {
        let _ = reply.send(result);
    }
    Ok(())
}
fn cleanup(shared: &Shared) {
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut status = None;
    while Instant::now() < deadline {
        let result = shared
            .process
            .lock()
            .expect("process owner")
            .as_mut()
            .map(|p| p.try_reap());
        match result {
            Some(Ok(Some(exit))) => {
                status = Some(exit);
                break;
            }
            Some(Ok(None)) => std::thread::sleep(Duration::from_millis(1)),
            _ => break,
        }
    }
    // Dropping an unreaped owner hands reaping to a runtime-independent OS waiter.
    let remaining = shared.process.lock().expect("process owner").take();
    drop(remaining);
    let mut r = shared.report.lock().expect("process diagnostics");
    r.reaped = status.is_some();
    r.cleanup_pending = r.pid.is_some() && status.is_none();
    r.exit_code = status.and_then(|s| s.code());
    r.signal = status.and_then(|s| s.signal());
    drop(r);
}
