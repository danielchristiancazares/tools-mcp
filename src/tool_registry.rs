use crate::RpcResponse;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// Trait for MCP tools. Each tool provides its definition and execution logic.
pub trait McpTool: Send + Sync + 'static {
    /// Primary tool name.
    const NAME: &'static str;

    /// Additional name aliases for backwards compatibility.
    const ALIASES: &'static [&'static str] = &[];

    /// Tool description shown in tool listings.
    const DESCRIPTION: &'static str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema() -> Value;

    /// Execute the tool with the given arguments.
    fn execute(
        id: Option<Value>,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>>;
}

/// Tool definition for MCP protocol responses.
#[derive(Clone, serde::Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

type ToolExecutor =
    Box<dyn Fn(Option<Value>, Value) -> Pin<Box<dyn Future<Output = RpcResponse<'static>> + Send>> + Send + Sync>;

struct ToolEntry {
    def: ToolDef,
    executor: ToolExecutor,
}

/// Registry of all MCP tools with lookup by name or alias.
pub struct ToolRegistry {
    tools: Vec<ToolEntry>,
    by_name: HashMap<String, usize>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    /// Register a tool type.
    pub fn register<T: McpTool>(&mut self) {
        let idx = self.tools.len();

        let def = ToolDef {
            name: T::NAME.to_string(),
            description: T::DESCRIPTION.to_string(),
            input_schema: T::input_schema(),
        };

        let executor: ToolExecutor = Box::new(|id, args| T::execute(id, args));

        self.tools.push(ToolEntry { def, executor });

        // Register primary name
        self.by_name.insert(T::NAME.to_string(), idx);

        // Register aliases
        for alias in T::ALIASES {
            self.by_name.insert(alias.to_string(), idx);
        }
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
    ) -> Option<RpcResponse<'static>> {
        let idx = self.by_name.get(name)?;
        let entry = &self.tools[*idx];
        Some((entry.executor)(id, args).await)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Macro to register multiple tools at once.
#[macro_export]
macro_rules! register_tools {
    ($registry:expr, [$($tool:ty),* $(,)?]) => {
        $(
            $registry.register::<$tool>();
        )*
    };
}

/// Macro to generate McpTool struct and impl with minimal boilerplate.
///
/// This macro automatically generates an empty struct and McpTool trait
/// implementation, reducing ~13 lines of boilerplate code per tool.
///
/// # Usage
///
/// ```ignore
/// define_mcp_tool! {
///     MyToolName {
///         name = "my-tool",
///         aliases = ["my", "tool"],
///         description = "Description of the tool",
///         handler = handle_my_tool
///     }
/// }
/// ```
///
/// Then define the input_schema separately:
///
/// ```ignore
/// impl McpTool for MyToolName {
///     fn input_schema() -> Value {
///         json!({...})
///     }
/// }
/// ```
///
/// # What It Generates
///
/// - Public struct declaration
/// - Full McpTool implementation with execute wrapper
/// - NAME, ALIASES, DESCRIPTION constants
/// - Requires you to implement input_schema() separately
///
/// # Expected Reduction
///
/// ~10-13 lines per tool × 15+ tools = ~150-200 total lines removed
#[macro_export]
macro_rules! define_mcp_tool {
    (
        $struct_name:ident {
            name = $tool_name:expr,
            aliases = [$($alias:expr),* $(,)?],
            description = $desc:expr,
            handler = $handler:path
        }
    ) => {
        pub struct $struct_name;

        impl $crate::tool_registry::McpTool for $struct_name {
            const NAME: &'static str = $tool_name;
            const ALIASES: &'static [&'static str] = &[$($alias),*];
            const DESCRIPTION: &'static str = $desc;

            fn execute(
                id: Option<$crate::serde_json::Value>,
                args: $crate::serde_json::Value,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = $crate::RpcResponse<'static>> + Send>
            > {
                Box::pin(async move { $handler(id, args).await })
            }
        }
    };
}
