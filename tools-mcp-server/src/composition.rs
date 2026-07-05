//! Composition root: wire feature crates into a single MCP tool registry.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use tools_mcp_core::ToolRegistry;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum JsonRpcId {
    String(String),
    Number(String),
    Null,
}

impl JsonRpcId {
    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::String(value) => Some(Self::String(value.clone())),
            Value::Number(value) => Some(Self::Number(value.to_string())),
            Value::Null => Some(Self::Null),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
pub struct InflightRegistry {
    inner: Arc<Mutex<HashMap<JsonRpcId, CancellationToken>>>,
}

pub struct InflightDropGuard {
    registry: InflightRegistry,
    id: JsonRpcId,
}

impl InflightRegistry {
    pub fn register(&self, id: JsonRpcId, token: CancellationToken) {
        self.inner
            .lock()
            .expect("inflight registry lock should not be poisoned")
            .insert(id, token);
    }

    pub fn cancel(&self, id: &JsonRpcId) -> bool {
        let token = self
            .inner
            .lock()
            .expect("inflight registry lock should not be poisoned")
            .get(id)
            .cloned();

        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub fn drop_on_completion(&self, id: JsonRpcId) -> InflightDropGuard {
        InflightDropGuard {
            registry: self.clone(),
            id,
        }
    }

    fn remove(&self, id: &JsonRpcId) {
        self.inner
            .lock()
            .expect("inflight registry lock should not be poisoned")
            .remove(id);
    }
}

impl Drop for InflightDropGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.id);
    }
}

/// Constructs the tool registry with all available MCP tools.
pub fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register::<crate::ping::PingTool>();
    tools_mcp_ado::register_tools(&mut registry);
    tools_mcp_webfetch::register_tools(&mut registry);
    tools_mcp_local::register_tools(&mut registry);
    tools_mcp_semantic::register_tools(&mut registry);
    tools_mcp_git::register_tools(&mut registry);

    registry
}
