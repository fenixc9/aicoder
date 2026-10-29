use super::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct MemoryArtifactStore {
    limits: ArtifactLimits,
    state: Arc<Mutex<State>>,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, (ArtifactRef, Vec<u8>)>,
    pending: usize,
}
impl MemoryArtifactStore {
    pub fn new(limits: ArtifactLimits) -> Result<Self, ArtifactError> {
        Ok(Self {
            limits: limits.validate()?,
            state: Arc::new(Mutex::new(State::default())),
        })
    }
}
impl Default for MemoryArtifactStore {
    fn default() -> Self {
        Self::new(ArtifactLimits::default()).expect("default artifact limits")
    }
}
struct Writer {
    store: MemoryArtifactStore,
    id: String,
    media_type: String,
    bytes: Vec<u8>,
    reserved: bool,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if self.reserved {
            self.store.state.lock().expect("artifacts").pending -= 1;
        }
    }
}
#[async_trait]
impl ArtifactStore for MemoryArtifactStore {
    fn limits(&self) -> ArtifactLimits {
        self.limits
    }
    async fn begin(&self, media_type: &str) -> Result<Box<dyn ArtifactWriter>, ArtifactError> {
        let id = uuid::Uuid::new_v4().to_string();
        ArtifactRef {
            version: 1,
            id: id.clone(),
            media_type: media_type.into(),
            bytes: 0,
            complete: false,
        }
        .validate()?;
        let mut state = self.state.lock().expect("artifacts");
        let used = state.entries.values().map(|(r, _)| r.bytes).sum::<u64>();
        if state.entries.len() + state.pending >= self.limits.max_artifacts
            || used.saturating_add(
                (state.pending as u64 + 1).saturating_mul(self.limits.max_artifact_bytes),
            ) > self.limits.max_total_bytes
        {
            return Err(ArtifactError::new("quota_exceeded"));
        }
        state.pending += 1;
        Ok(Box::new(Writer {
            store: self.clone(),
            id,
            media_type: media_type.into(),
            bytes: Vec::new(),
            reserved: true,
        }))
    }
    async fn read(
        &self,
        id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, ArtifactError> {
        if !valid_id(id) {
            return Err(ArtifactError::new("invalid_reference"));
        }
        let state = self.state.lock().expect("artifacts");
        let (artifact, bytes) = state
            .entries
            .get(id)
            .ok_or(ArtifactError::new("not_found"))?;
        let count = page_bounds(artifact.bytes, offset, limit)?;
        let end = offset as usize + count;
        Ok(ArtifactPage {
            artifact: artifact.clone(),
            offset,
            bytes: bytes[offset as usize..end].to_vec(),
            next_offset: (end < bytes.len()).then_some(end as u64),
        })
    }
    async fn remove(&self, id: &str) -> Result<(), ArtifactError> {
        if !valid_id(id) {
            return Err(ArtifactError::new("invalid_reference"));
        }
        self.state
            .lock()
            .expect("artifacts")
            .entries
            .remove(id)
            .ok_or(ArtifactError::new("not_found"))?;
        Ok(())
    }
}
#[async_trait]
impl ArtifactWriter for Writer {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ArtifactError> {
        if (self.bytes.len() as u64).saturating_add(bytes.len() as u64)
            > self.store.limits.max_artifact_bytes
        {
            return Err(ArtifactError::new("too_large"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    async fn finish(mut self: Box<Self>, complete: bool) -> Result<ArtifactRef, ArtifactError> {
        let reference = ArtifactRef {
            version: 1,
            id: self.id.clone(),
            media_type: self.media_type.clone(),
            bytes: self.bytes.len() as u64,
            complete,
        };
        let mut state = self.store.state.lock().expect("artifacts");
        state.pending -= 1;
        self.reserved = false;
        state.entries.insert(
            self.id.clone(),
            (reference.clone(), std::mem::take(&mut self.bytes)),
        );
        Ok(reference)
    }
}
