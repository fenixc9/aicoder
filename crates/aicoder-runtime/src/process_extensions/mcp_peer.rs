//! Bounded transport-side MCP controls. No model, filesystem-root or UI authority is exposed.
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{
        Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
pub(crate) const PROBING: u8 = 0;
pub(crate) const LEGACY: u8 = 1;
pub(crate) const MODERN: u8 = 2;
#[derive(Debug, Clone, Default, Serialize)]
pub struct McpWireDiagnostics {
    pub notifications_received: u64,
    pub notifications_written: u64,
    pub legacy_pings: u64,
    pub rejected_server_requests: u64,
    pub control_replies_written: u64,
    pub cancellation_written: u64,
    pub catalog_epoch: u64,
}
pub(crate) struct McpPeer {
    pub era: AtomicU8,
    pub epoch: AtomicU64,
    stats: Mutex<McpWireDiagnostics>,
    controls: Mutex<VecDeque<(Vec<u8>, usize)>>,
    cancelled: Mutex<Option<u64>>,
    retired: Mutex<Vec<u64>>,
    supported: Mutex<Vec<String>>,
    window: Mutex<(Instant, usize, usize)>,
    max_messages: usize,
    max_bytes: usize,
}
impl McpPeer {
    pub fn new(max_messages: usize, max_bytes: usize) -> Self {
        Self {
            era: AtomicU8::new(PROBING),
            epoch: AtomicU64::new(0),
            stats: Mutex::new(Default::default()),
            controls: Mutex::new(VecDeque::new()),
            cancelled: Mutex::new(None),
            retired: Mutex::new(Vec::new()),
            supported: Mutex::new(Vec::new()),
            window: Mutex::new((Instant::now(), 0, 0)),
            max_messages,
            max_bytes,
        }
    }
    pub fn diagnostics(&self) -> McpWireDiagnostics {
        let mut stats = self.stats.lock().expect("MCP diagnostics").clone();
        stats.catalog_epoch = self.epoch.load(Ordering::SeqCst);
        stats
    }
    pub fn cancel(&self, id: u64) {
        *self.cancelled.lock().expect("MCP cancellation") = Some(id);
    }
    pub fn cancellation(&self) -> Option<u64> {
        self.cancelled.lock().expect("MCP cancellation").take()
    }
    pub fn cancellation_written(&self) {
        self.stats
            .lock()
            .expect("MCP diagnostics")
            .cancellation_written += 1;
    }
    pub fn retire(&self, id: u64) {
        let mut ids = self.retired.lock().expect("MCP probe");
        if ids.len() < 2 {
            ids.push(id);
        }
    }
    pub fn supported_versions(&self) -> Vec<String> {
        self.supported.lock().expect("MCP versions").clone()
    }
    pub fn remote_error(&self, error: &Value) {
        if error["code"] == -32022 {
            let values = error["data"]["supported"].as_array();
            if let Some(values) = values.filter(|v| v.len() <= 8) {
                let versions = values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| {
                        s.len() == 10
                            && s.bytes().enumerate().all(|(i, b)| {
                                if i == 4 || i == 7 {
                                    b == b'-'
                                } else {
                                    b.is_ascii_digit()
                                }
                            })
                    })
                    .map(str::to_owned)
                    .collect();
                *self.supported.lock().expect("MCP versions") = versions;
            }
        }
    }
    fn charge(&self, bytes: usize) -> Result<(), ()> {
        let mut window = self.window.lock().map_err(|_| ())?;
        if window.0.elapsed() >= Duration::from_secs(1) {
            *window = (Instant::now(), 0, 0);
        }
        window.1 = window.1.saturating_add(1);
        window.2 = window.2.saturating_add(bytes);
        if window.1 > self.max_messages || window.2 > self.max_bytes {
            return Err(());
        }
        Ok(())
    }
    pub fn auxiliary(&self, value: &Value, bytes: usize) -> Result<bool, ()> {
        let object = value.as_object().ok_or(())?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(());
        }
        if !object.contains_key("method") {
            if let Some(id) = object.get("id").and_then(Value::as_u64) {
                let mut retired = self.retired.lock().map_err(|_| ())?;
                if let Some(index) = retired.iter().position(|old| *old == id) {
                    retired.remove(index);
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if object.contains_key("result") || object.contains_key("error") {
            return Err(());
        }
        let method = object.get("method").and_then(Value::as_str).ok_or(())?;
        self.charge(bytes)?;
        if let Some(id) = object.get("id") {
            if self.era.load(Ordering::SeqCst) == MODERN
                || !(id.as_i64().is_some()
                    || id.as_u64().is_some()
                    || id.as_str().is_some_and(|s| s.len() <= 128))
            {
                return Err(());
            }
            let reply = if method == "ping" {
                self.stats.lock().map_err(|_| ())?.legacy_pings += 1;
                json!({"jsonrpc":"2.0","id":id,"result":{}})
            } else {
                self.stats.lock().map_err(|_| ())?.rejected_server_requests += 1;
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"MCP client capability is not enabled"}})
            };
            let mut frame = serde_json::to_vec(&reply).map_err(|_| ())?;
            frame.push(b'\n');
            let mut controls = self.controls.lock().map_err(|_| ())?;
            if controls.len() >= 8 {
                return Err(());
            }
            controls.push_back((frame, 0));
            return Ok(true);
        }
        self.stats.lock().map_err(|_| ())?.notifications_received += 1;
        if method == "notifications/tools/list_changed" {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
        // Unknown notifications are inert data. No instructions, requests or grants are applied.
        Ok(true)
    }
    pub fn controls_empty(&self) -> bool {
        self.controls.lock().expect("MCP controls").is_empty()
    }
    pub fn flush(&self, writer: &mut impl Write) -> io::Result<()> {
        let mut controls = self.controls.lock().expect("MCP controls");
        if let Some((bytes, offset)) = controls.front_mut() {
            match writer.write(&bytes[*offset..]) {
                Ok(0) => return Err(io::Error::other("MCP control pipe closed")),
                Ok(n) => {
                    *offset += n;
                    if *offset == bytes.len() {
                        controls.pop_front();
                        self.stats
                            .lock()
                            .expect("MCP diagnostics")
                            .control_replies_written += 1;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    pub fn notification_written(&self) {
        self.stats
            .lock()
            .expect("MCP diagnostics")
            .notifications_written += 1;
    }
}
