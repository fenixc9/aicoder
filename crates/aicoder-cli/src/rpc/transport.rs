//! Bounded serialization and nonblocking stdout, without an orphan writer thread.
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FRAMES: usize = 256;
pub const STALL: Duration = Duration::from_secs(5);
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME.saturating_sub(self.0.len()) {
            return Err(io::Error::other("RPC frame exceeds 8 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub struct Output {
    file: File,
    queue: VecDeque<Vec<u8>>,
    offset: usize,
    bytes: usize,
    progressed: Instant,
}
impl Output {
    pub fn new() -> Result<Self> {
        Ok(Self {
            file: OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/dev/stdout")?,
            queue: VecDeque::new(),
            offset: 0,
            bytes: 0,
            progressed: Instant::now(),
        })
    }
    pub fn push(&mut self, value: &impl Serialize) -> Result<()> {
        let mut buffer = Bounded(Vec::new());
        serde_json::to_writer(&mut buffer, value)?;
        buffer.write_all(b"\n")?;
        ensure!(
            self.queue.len() < MAX_FRAMES && buffer.0.len() <= MAX_BYTES.saturating_sub(self.bytes),
            "RPC output queue exhausted"
        );
        if self.queue.is_empty() {
            self.progressed = Instant::now();
        }
        self.bytes += buffer.0.len();
        self.queue.push_back(buffer.0);
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn flush_some(&mut self) -> Result<()> {
        // Bound work per reactor turn so a readable/writable transport cannot starve controls.
        let mut budget = 64 * 1024;
        while let Some(front) = self.queue.front() {
            if budget == 0 {
                break;
            }
            let end = (self.offset + budget).min(front.len());
            match self.file.write(&front[self.offset..end]) {
                Ok(0) => anyhow::bail!("RPC stdout closed"),
                Ok(n) => {
                    self.offset += n;
                    self.bytes -= n;
                    budget -= n;
                    self.progressed = Instant::now();
                    if self.offset == front.len() {
                        self.queue.pop_front();
                        self.offset = 0;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        ensure!(
            self.is_empty() || self.progressed.elapsed() < STALL,
            "RPC stdout made no progress for 5 seconds"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialization_limits_encoded_bytes_and_queue_recovers_after_drain() {
        let file = tempfile::tempfile().unwrap();
        let mut out = Output {
            file,
            queue: VecDeque::new(),
            offset: 0,
            bytes: 0,
            progressed: Instant::now(),
        };
        // Escaping doubles these payload bytes before a frame could enter the queue.
        assert!(out.push(&"\n".repeat(MAX_FRAME / 2)).is_err());
        assert!(out.is_empty());
        for _ in 0..MAX_FRAMES {
            out.push(&true).unwrap();
        }
        assert!(out.push(&true).is_err());
        out.flush_some().unwrap();
        assert!(out.is_empty());
        out.push(&false).unwrap();
        out.flush_some().unwrap();
        assert_eq!(out.bytes, 0);
    }
}
