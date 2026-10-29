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
        let name = tool.definition().name;
        if self.indexes.contains_key(&name) {
            anyhow::bail!("Tool already registered: {name}");
        }
        self.indexes.insert(name, self.tools.len());
        self.tools.push(Arc::new(tool));
        Ok(())
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
