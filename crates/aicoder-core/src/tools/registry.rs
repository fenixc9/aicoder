use std::{collections::HashMap, sync::Arc};

use anyhow::Result;

use crate::model::ToolDefinition;

use super::ExecutableTool;

pub struct ToolRegistry {
    tools: Vec<Arc<dyn ExecutableTool>>,
    indexes: HashMap<String, usize>,
    pub(crate) authority: uuid::Uuid,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            indexes: HashMap::new(),
            authority: uuid::Uuid::new_v4(),
        }
    }
}

impl ToolRegistry {
    pub fn register<T>(&mut self, tool: T) -> Result<()>
    where
        T: ExecutableTool + 'static,
    {
        self.register_shared(Arc::new(tool))
    }

    pub fn register_shared(&mut self, tool: Arc<dyn ExecutableTool>) -> Result<()> {
        let name = tool.definition().name;
        if self.indexes.contains_key(&name) {
            anyhow::bail!("Tool already registered: {name}");
        }
        self.indexes.insert(name, self.tools.len());
        self.tools.push(tool);
        Ok(())
    }

    /// Fork an immutable registry with fresh authority; conflicting additions publish nothing.
    pub fn with_additions(
        &self,
        additions: impl IntoIterator<Item = Arc<dyn ExecutableTool>>,
    ) -> Result<Self> {
        let mut next = Self::default();
        for tool in self.tools.iter().cloned().chain(additions) {
            next.register_shared(tool)?;
        }
        Ok(next)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ExecutableTool>> {
        self.indexes
            .get(name)
            .and_then(|index| self.tools.get(*index))
            .cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }
}
