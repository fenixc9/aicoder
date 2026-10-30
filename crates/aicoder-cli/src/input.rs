//! One owned nonblocking input description; no orphan blocking stdin readers.
use anyhow::{Result, ensure};
use std::{
    fs::{File, OpenOptions},
    io::{ErrorKind, Read},
    os::unix::fs::OpenOptionsExt,
    time::Duration,
};
#[derive(Debug)]
pub struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ctrl-C")
    }
}
impl std::error::Error for Interrupted {}
pub const MAX_INPUT: usize = 1024 * 1024;
pub fn open(terminal: bool) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(if terminal { "/dev/tty" } else { "/dev/stdin" })?)
}
pub struct Lines {
    file: File,
    pending: Vec<u8>,
    eof: bool,
}
impl Lines {
    pub fn new() -> Result<Self> {
        Ok(Self {
            file: open(false)?,
            pending: Vec::new(),
            eof: false,
        })
    }
    pub async fn line(
        &mut self,
        signal: &mut tokio::signal::unix::Signal,
    ) -> Result<Option<String>> {
        tokio::select! {
            _=signal.recv()=>Err(Interrupted.into()),
            line=self.next_line()=>line,
        }
    }
    pub async fn next_line(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                ensure!(end < MAX_INPUT, "stdin line exceeds 1 MiB");
                let line = self.pending.drain(..=end).collect::<Vec<_>>();
                return Ok(Some(String::from_utf8(line)?));
            }
            ensure!(self.pending.len() <= MAX_INPUT, "stdin line exceeds 1 MiB");
            if self.eof {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(String::from_utf8(std::mem::take(&mut self.pending))?))
                };
            }
            let mut buf = [0; 4096];
            match self.file.read(&mut buf) {
                Ok(0) => self.eof = true,
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    pub async fn all(&mut self, signal: &mut tokio::signal::unix::Signal) -> Result<String> {
        let mut text = String::new();
        while let Some(line) = self.line(signal).await? {
            ensure!(
                text.len().saturating_add(line.len()) <= MAX_INPUT,
                "stdin prompt exceeds 1 MiB"
            );
            text.push_str(&line);
        }
        Ok(text)
    }
}
pub fn approval_line(
    terminal: bool,
    sender: &tokio::sync::oneshot::Sender<Result<aicoder_core::tools::ApprovalDecision>>,
) -> Result<Option<String>> {
    let mut file = match open(terminal) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let mut bytes = Vec::new();
    loop {
        if sender.is_closed() {
            return Ok(None);
        }
        let mut byte = [0];
        match file.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Ok(Some(String::from_utf8(bytes)?));
                }
                ensure!(bytes.len() < 4096, "approval answer too long");
                bytes.push(byte[0]);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}
