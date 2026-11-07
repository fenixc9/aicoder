//! Ephemeral positional lineage, built from real session IDs rather than text matching.
use crate::{model::Message, session::OwnedSessionContext};
use anyhow::{Result, ensure};
use std::collections::HashMap;
/// A validated correspondence between one model view and its canonical transcript.
/// This is data, not repository/IO authority, and is never serialized into history.
#[derive(Debug, Clone)]
pub struct CompactionProjection {
    positions: Vec<Option<usize>>,
    history_len: usize,
    prefix: usize,
    view_digest: String,
    history_digest: String,
    uncertain: bool,
}
impl CompactionProjection {
    /// Equality validates this exact view; IDs, not equal message text, select history positions.
    /// A different view has no admitted lineage and retains the legacy suffix behavior.
    pub fn from_session_view<'a>(
        source: &OwnedSessionContext,
        view: &[Message],
        prefix: usize,
        history: &[Message],
        positions: impl Iterator<Item = (&'a str, usize)>,
    ) -> Result<Option<Self>> {
        ensure!(
            source.messages.len() == source.entry_ids.len(),
            "invalid context origin count"
        );
        if view.get(prefix..) != Some(source.messages.as_slice()) {
            return Ok(None);
        }
        let history_len = history.len();
        let positions = positions.collect::<HashMap<_, _>>();
        ensure!(
            positions.values().all(|i| *i >= prefix && *i < history_len),
            "invalid canonical context positions"
        );
        if source
            .entry_ids
            .iter()
            .flatten()
            .any(|id| !positions.contains_key(id.as_str()))
        {
            return Ok(None);
        }
        let mut mapped = vec![None; prefix];
        mapped.extend(
            source
                .entry_ids
                .iter()
                .map(|id| id.as_deref().and_then(|id| positions.get(id).copied())),
        );
        Ok(Some(Self {
            positions: mapped,
            history_len,
            prefix,
            view_digest: super::digest(view),
            history_digest: super::digest(history),
            uncertain: source.source.interrupted_turns > 0
                || source.source.recovered_tool_results > 0,
        }))
    }
    pub fn canonical_index(&self, view_index: usize) -> Option<usize> {
        self.positions.get(view_index).copied().flatten()
    }
    pub fn retained_messages(&self, view_index: usize) -> Option<usize> {
        self.history_len
            .checked_sub(self.canonical_index(view_index)?)
    }
    pub fn uncertain_operations(&self) -> bool {
        self.uncertain
    }
    pub(crate) fn validate(
        &self,
        view: &[Message],
        history: &[Message],
        prefix: usize,
    ) -> Result<()> {
        ensure!(
            self.positions.len() == view.len()
                && self.history_len == history.len()
                && self.prefix == prefix
                && self.view_digest == super::digest(view)
                && self.history_digest == super::digest(history),
            "stale compaction projection"
        );
        Ok(())
    }
    pub(crate) fn selected(&self, keep: &[usize], view: &[Message]) -> Self {
        Self {
            positions: keep.iter().map(|i| self.positions[*i]).collect(),
            history_len: self.history_len,
            prefix: self.prefix,
            view_digest: super::digest(view),
            history_digest: self.history_digest.clone(),
            uncertain: self.uncertain,
        }
    }
}
