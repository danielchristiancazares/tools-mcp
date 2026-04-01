use crate::tool_outcome::ToolCallOutcome;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

/// Trait for MCP tools. Each tool provides its definition and execution logic.
pub trait McpTool: Send + Sync + 'static {
    /// Primary tool name.
    const NAME: &'static str;

    /// Tool description shown in tool listings.
    const DESCRIPTION: &'static str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema() -> Value;

    /// Execute the tool with the given arguments.
    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>>;
}

/// Tool definition for MCP protocol responses.
#[derive(Clone, serde::Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

type ToolExecutor = Box<
    dyn Fn(Option<Value>, Value) -> Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>>
        + Send
        + Sync,
>;

struct ToolEntry {
    def: ToolDef,
    executor: ToolExecutor,
}

/// Registry of all MCP tools with lookup by canonical name.
pub struct ToolRegistry {
    tools: Vec<ToolEntry>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register a tool type.
    pub fn register<T: McpTool>(&mut self) {
        let def = ToolDef {
            name: T::NAME.to_string(),
            description: T::DESCRIPTION.to_string(),
            input_schema: T::input_schema(),
        };

        let executor: ToolExecutor = Box::new(|id, args| T::execute(id, args));

        self.tools.push(ToolEntry { def, executor });
    }

    /// Get all tool definitions for protocol responses.
    pub fn list(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|e| e.def.clone()).collect()
    }

    /// Look up and execute a tool by name. Returns None if tool not found.
    pub async fn call(
        &self,
        name: &str,
        id: Option<Value>,
        args: Value,
    ) -> Option<crate::RpcResponse<'static>> {
        let entry = self.tools.iter().find(|entry| entry.def.name == name)?;
        let outcome = (entry.executor)(id.clone(), args).await;
        Some(outcome.into_rpc_response(id))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::define_mcp_tool;
    use serde_json::json;

    async fn ok_tool(_id: Option<Value>, _args: Value) -> ToolCallOutcome {
        ToolCallOutcome::ok(json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false
        }))
    }

    define_mcp_tool! {
        DummyTool,
        name: "Dummy",
        description: "dummy tool for registry tests",
        schema: {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        handler: ok_tool
    }

    #[tokio::test]
    async fn registry_dispatches_by_name() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert!(reg.list().iter().any(|t| t.name == "Dummy"));

        let r1 = reg.call("Dummy", Some(json!(1)), json!({})).await;
        assert!(r1.is_some());
    }

    #[tokio::test]
    async fn registry_returns_none_for_unknown_tool() {
        let reg = ToolRegistry::new();
        let r = reg.call("nope", Some(json!(1)), json!({})).await;
        assert!(r.is_none());
    }
}

/// Macro to define an MCP tool with reduced boilerplate.
///
/// # Syntax
///
/// ```ignore
/// define_mcp_tool! {
///     /// Optional doc comment for the tool struct
///     ToolName,
///     name: "ToolName",
///     description: "Tool description",
///     schema: { "type": "object", ... },
///     handler: handler_function
/// }
/// ```
///
/// # Example
///
/// ```ignore
/// define_mcp_tool! {
///     /// Reads file contents with optional line range.
///     ReadTool,
///     name: "Read",
///     description: "Read file contents with optional line range",
///     schema: {
///         "type": "object",
///         "properties": {
///             "path": { "type": "string", "description": "Path to read" }
///         },
///         "required": ["path"]
///     },
///     handler: handle_read_file
/// }
/// ```
#[macro_export]
macro_rules! define_mcp_tool {
    (
        $(#[$meta:meta])*
        $tool:ident,
        name: $name:expr,
        description: $desc:expr,
        schema: $schema:tt,
        handler: $handler:expr
    ) => {
        $(#[$meta])*
        pub struct $tool;

        impl $crate::tool_registry::McpTool for $tool {
            const NAME: &'static str = $name;
            const DESCRIPTION: &'static str = $desc;

            fn input_schema() -> serde_json::Value {
                serde_json::json!($schema)
            }

            fn execute(
                id: Option<serde_json::Value>,
                args: serde_json::Value,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = $crate::tool_outcome::ToolCallOutcome> + Send>> {
                Box::pin($handler(id, args))
            }
        }
    };
}
