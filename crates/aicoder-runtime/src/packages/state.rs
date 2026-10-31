use super::*;
use aicoder_core::{
    TurnExecutionContext,
    session::{SessionEntryData, SessionRepository},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
pub const PACKAGE_STATE_NAMESPACE: &str = "aicoder.packages.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSessionState {
    pub version: u32,
    pub pins: Vec<PackagePin>,
    #[serde(default)]
    pub public: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
}
impl PackageSessionState {
    /// Read an explicit public lock/state document with a fixed serialized byte ceiling.
    pub fn read(path: &Path) -> Result<Self> {
        let (_, bytes) = crate::files::read_bounded(path, 32 * 1024, true)?
            .context("package state file missing")?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub fn capture(snapshot: &PackageSnapshot) -> Self {
        Self {
            version: 1,
            pins: snapshot.pins(),
            public: BTreeMap::new(),
        }
    }
    pub fn validate(&self, store: &PackageStore) -> Result<PackageSnapshot> {
        ensure!(
            self.version == 1 && self.pins.len() <= 32 && self.public.len() <= 32,
            "package session state version/byte budget exceeded"
        );
        super::bounded_json(self, 32 * 1024)?;
        let snapshot = store.snapshot(None, Some(&self.pins))?;
        for (id, state) in &self.public {
            snapshot
                .get(id)
                .context("state names an unselected package")?
                .manifest
                .validate_state(state)?;
        }
        Ok(snapshot)
    }
    pub fn save(
        &self,
        store: &PackageStore,
        repository: &dyn SessionRepository,
        workspace: &Path,
        id: &str,
        expected_revision: u64,
        context: &TurnExecutionContext,
    ) -> Result<()> {
        self.validate(store)?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let mut session = repository.open_run(id)?;
        ensure!(
            session.metadata().cwd == workspace && session.metadata().revision == expected_revision,
            "stale or foreign package session state"
        );
        let _lease = session.acquire(repository)?;
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        session.append(
            repository,
            vec![SessionEntryData::Extension {
                namespace: PACKAGE_STATE_NAMESPACE.into(),
                data: serde_json::to_value(self)?,
            }],
        )?;
        Ok(())
    }
    pub fn restore(
        store: &PackageStore,
        repository: &dyn SessionRepository,
        workspace: &Path,
        id: &str,
    ) -> Result<Option<Self>> {
        let Some(record) = repository.extension_state(id, PACKAGE_STATE_NAMESPACE, 32 * 1024)?
        else {
            return Ok(None);
        };
        ensure!(
            record.metadata.cwd == workspace,
            "package state belongs to another workspace"
        );
        let state: Self = serde_json::from_value(record.data)?;
        state.validate(store)?;
        Ok(Some(state))
    }
}
