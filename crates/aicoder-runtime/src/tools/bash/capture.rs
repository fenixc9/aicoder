use super::process::{ManagedChild, Stop};
use aicoder_core::artifacts::{ArtifactRef, ArtifactStore, ArtifactWriter};
use std::sync::Arc;
use std::{collections::VecDeque, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{ChildStderr, ChildStdout},
    signal::unix::Signal,
    time::Instant,
};
const CHUNK: usize = 8192;
pub(super) struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    limit: usize,
    archive_timeout: Duration,
    pub seen: u64,
    pub eof: bool,
    store: Option<Arc<dyn ArtifactStore>>,
    writer: Option<Box<dyn ArtifactWriter>>,
    archived: u64,
    limited: bool,
    pub artifact: Option<ArtifactRef>,
    pub artifact_error: Option<String>,
}
pub(super) struct View {
    pub text: String,
    pub omitted: u64,
}
impl Capture {
    fn new(limit: usize) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            limit,
            archive_timeout: Duration::from_millis(250),
            seen: 0,
            eof: false,
            store: None,
            writer: None,
            archived: 0,
            limited: false,
            artifact: None,
            artifact_error: None,
        }
    }
    async fn push(&mut self, mut bytes: &[u8]) {
        if self.store.is_some()
            && self.artifact_error.is_none()
            && self.seen.saturating_add(bytes.len() as u64) > self.limit as u64
        {
            self.start_archive().await;
        }
        self.append_archive(bytes).await;
        self.seen += bytes.len() as u64;
        let count = bytes
            .len()
            .min((self.limit / 2).saturating_sub(self.head.len()));
        self.head.extend_from_slice(&bytes[..count]);
        bytes = &bytes[count..];
        let limit = self.limit - self.limit / 2;
        if limit == 0 {
            return;
        }
        if bytes.len() >= limit {
            self.tail.clear();
            self.tail.extend(&bytes[bytes.len() - limit..]);
        } else {
            let remove = (self.tail.len() + bytes.len()).saturating_sub(limit);
            self.tail.drain(..remove);
            self.tail.extend(bytes);
        }
    }
    async fn start_archive(&mut self) {
        if self.writer.is_some() || self.artifact_error.is_some() {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        match tokio::time::timeout(
            self.archive_timeout,
            store.begin("application/octet-stream"),
        )
        .await
        {
            Ok(Ok(writer)) => {
                self.writer = Some(writer);
                let prefix = self
                    .head
                    .iter()
                    .chain(self.tail.iter())
                    .copied()
                    .collect::<Vec<_>>();
                self.append_archive(&prefix).await;
            }
            Ok(Err(error)) => self.artifact_error = Some(error.code.into()),
            Err(_) => self.artifact_error = Some("storage_timeout".into()),
        }
    }
    async fn append_archive(&mut self, bytes: &[u8]) {
        let Some(writer) = &mut self.writer else {
            return;
        };
        let limit = self
            .store
            .as_ref()
            .expect("artifact writer store")
            .limits()
            .max_artifact_bytes;
        let count = (limit.saturating_sub(self.archived)).min(bytes.len() as u64) as usize;
        self.limited |= count < bytes.len();
        if count == 0 {
            return;
        }
        match tokio::time::timeout(self.archive_timeout, writer.write(&bytes[..count])).await {
            Ok(Ok(())) => self.archived += count as u64,
            result => {
                self.artifact_error = Some(
                    match result {
                        Ok(Err(error)) => error.code,
                        _ => "storage_timeout",
                    }
                    .into(),
                );
                self.writer = None;
            }
        }
    }
    pub async fn finalize_archive(&mut self) {
        if self.seen == 0 {
            return;
        }
        self.start_archive().await;
        if let Some(writer) = self.writer.take() {
            match tokio::time::timeout(
                self.archive_timeout,
                writer.finish(self.eof && !self.limited),
            )
            .await
            {
                Ok(Ok(reference)) => self.artifact = Some(reference),
                Ok(Err(error)) => self.artifact_error = Some(error.code.into()),
                Err(_) => self.artifact_error = Some("storage_timeout".into()),
            }
        }
    }
    pub fn view(&self, limit: usize) -> View {
        let stored = self.head.len() + self.tail.len();
        let mut bytes = Vec::new();
        if self.seen == stored as u64 && stored <= limit {
            bytes.extend(&self.head);
            bytes.extend(&self.tail);
            return View {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                omitted: 0,
            };
        }
        let head = self.head.len().min(limit / 2);
        let tail = self.tail.len().min(limit - head);
        bytes.extend(&self.head[..head]);
        trim_incomplete_end(&mut bytes);
        let mut last = self
            .tail
            .iter()
            .skip(self.tail.len() - tail)
            .copied()
            .collect::<Vec<_>>();
        let start = last
            .iter()
            .position(|byte| byte & 0xc0 != 0x80)
            .unwrap_or(last.len());
        last.drain(..start);
        trim_incomplete_end(&mut last);
        let omitted = self.seen - (bytes.len() + last.len()) as u64;
        if bytes.is_empty() && last.is_empty() {
            return View {
                text: String::new(),
                omitted,
            };
        }
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if omitted > 0 {
            text.push_str(&format!("\n[… {omitted} bytes omitted …]\n"));
        }
        text.push_str(&String::from_utf8_lossy(&last));
        View { text, omitted }
    }
}
fn trim_incomplete_end(bytes: &mut Vec<u8>) {
    if let Err(error) = std::str::from_utf8(bytes)
        && error.error_len().is_none()
    {
        bytes.truncate(error.valid_up_to());
    }
}

pub(super) struct Captures {
    pub stdout: Capture,
    pub stderr: Capture,
    pub spawned: bool,
    pub limit_hit: bool,
}
impl Captures {
    pub fn new(limit: usize) -> Self {
        Self {
            stdout: Capture::new(limit),
            stderr: Capture::new(limit),
            spawned: false,
            limit_hit: false,
        }
    }
    pub fn with_archive_timeout(mut self, timeout: Duration) -> Self {
        self.stdout.archive_timeout = timeout;
        self.stderr.archive_timeout = timeout;
        self
    }
    pub fn with_store(mut self, store: Option<Arc<dyn ArtifactStore>>) -> Self {
        self.stdout.store = store.clone();
        self.stderr.store = store;
        self
    }
    pub fn complete(&self) -> bool {
        self.stdout.eof && self.stderr.eof
    }
    fn seen(&self) -> u64 {
        self.stdout.seen + self.stderr.seen
    }
}
enum Read {
    Out(std::io::Result<usize>),
    Err(std::io::Result<usize>),
}
async fn read_any(
    stdout: &mut ChildStdout,
    stderr: &mut ChildStderr,
    capture: &Captures,
    out: &mut [u8],
    err: &mut [u8],
) -> Read {
    tokio::select! {
        result=stdout.read(out),if !capture.stdout.eof=>Read::Out(result),
        result=stderr.read(err),if !capture.stderr.eof=>Read::Err(result),
        else=>std::future::pending().await,
    }
}
async fn consume(
    read: Read,
    out: &[u8],
    err: &[u8],
    capture: &mut Captures,
    limit: u64,
) -> Result<(), Stop> {
    let (capture_stream, result, bytes) = match read {
        Read::Out(result) => (&mut capture.stdout, result, out),
        Read::Err(result) => (&mut capture.stderr, result, err),
    };
    match result {
        Ok(0) => capture_stream.eof = true,
        Ok(count) => capture_stream.push(&bytes[..count]).await,
        Err(_) => return Err(Stop::CaptureError),
    }
    if capture.seen() > limit {
        capture.limit_hit = true;
        return Err(Stop::OutputLimit);
    }
    Ok(())
}
pub(super) async fn drive(
    child: &mut ManagedChild,
    exits: &mut Signal,
    stdout: &mut ChildStdout,
    stderr: &mut ChildStderr,
    capture: &mut Captures,
    limit: u64,
    deadline: Instant,
) -> Stop {
    let (mut out, mut err) = ([0_u8; CHUNK], [0_u8; CHUNK]);
    loop {
        match child.exited() {
            Ok(true) => return Stop::Exit,
            Ok(false) => {}
            Err(_) => return Stop::WaitError,
        }
        let count = (limit.saturating_sub(capture.seen()) + 1).min(CHUNK as u64) as usize;
        tokio::select! {biased;
            _=tokio::time::sleep_until(deadline)=>return Stop::Timeout,
            signal=exits.recv()=>if signal.is_none(){return Stop::WaitError;},
            read=read_any(stdout,stderr,capture,&mut out[..count],&mut err[..count])=>if let Err(stop)=consume(read,&out,&err,capture,limit).await{return stop;},
        }
    }
}
pub(super) async fn drain(
    stdout: &mut ChildStdout,
    stderr: &mut ChildStderr,
    capture: &mut Captures,
    limit: u64,
) {
    let work = async {
        let (mut out, mut err) = ([0_u8; CHUNK], [0_u8; CHUNK]);
        while !capture.complete() {
            let count = (limit.saturating_sub(capture.seen()) + 1).min(CHUNK as u64) as usize;
            let read = read_any(
                stdout,
                stderr,
                capture,
                &mut out[..count],
                &mut err[..count],
            )
            .await;
            if consume(read, &out, &err, capture, limit).await.is_err() {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(250), work).await;
}
