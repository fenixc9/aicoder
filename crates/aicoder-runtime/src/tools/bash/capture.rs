use super::process::{ManagedChild, Stop};
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
    pub seen: u64,
    pub eof: bool,
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
            seen: 0,
            eof: false,
        }
    }
    fn push(&mut self, mut bytes: &[u8]) {
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
fn consume(
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
        Ok(count) => capture_stream.push(&bytes[..count]),
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
            read=read_any(stdout,stderr,capture,&mut out[..count],&mut err[..count])=>if let Err(stop)=consume(read,&out,&err,capture,limit){return stop;},
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
            if consume(read, &out, &err, capture, limit).is_err() {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(250), work).await;
}
