//! Branch-local model parameters. Never model instructions or restored host authority.
use super::*;
use crate::model::ReasoningEffort;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
pub const REASONING_NAMESPACE: &str = "aicoder.reasoning.v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningSelection {
    pub version: u32,
    pub provider: String,
    pub model: String,
    pub effort: Option<ReasoningEffort>,
}
impl ReasoningSelection {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        effort: Option<ReasoningEffort>,
    ) -> Self {
        Self {
            version: 1,
            provider: provider.into(),
            model: model.into(),
            effort,
        }
    }
    pub fn data(&self) -> SessionEntryData {
        SessionEntryData::Extension {
            namespace: REASONING_NAMESPACE.into(),
            data: serde_json::to_value(self).expect("typed reasoning choice"),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported reasoning selection version");
        navigation::name(&self.provider)?;
        navigation::name(&self.model)
    }
}
impl SessionBranch<'_> {
    pub fn reasoning_selection(&self) -> Result<Option<ReasoningSelection>> {
        for entry in self.entries().iter().rev() {
            match &entry.data {
                SessionEntryData::ModelChanged { .. } => break,
                SessionEntryData::Extension { namespace, data }
                    if namespace == REASONING_NAMESPACE =>
                {
                    let choice: ReasoningSelection = serde_json::from_value(data.clone())?;
                    choice.validate()?;
                    ensure!(
                        self.selected_model()
                            == Some((choice.provider.as_str(), choice.model.as_str())),
                        "reasoning selection belongs to another model epoch"
                    );
                    return Ok(Some(choice));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}
impl Session {
    pub fn reasoning_selection(&self) -> Result<Option<ReasoningSelection>> {
        self.selected_branch().reasoning_selection()
    }
}
impl RunSession {
    pub fn reasoning_selection(&self) -> Result<Option<ReasoningSelection>> {
        match self {
            Self::Full(session) => session.reasoning_selection(),
            Self::Window(window) => Ok(window.reasoning.clone()),
        }
    }
}
