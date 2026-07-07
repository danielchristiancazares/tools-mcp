use crate::cancellation::CURRENT_CANCEL_TOKEN;
use crate::tool_outcome::{DispatchOutcome, ToolCallOutcome};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Trait for MCP tools. Each tool provides its definition and execution logic.
pub trait McpTool: Send + Sync + 'static {
    /// Primary tool name.
    const NAME: &'static str;

    /// Alternate accepted names for inbound tool calls.
    const ALIASES: &'static [&'static str] = &[];

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

type ToolFuture = Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>>;
type ToolExecutor = fn(Option<Value>, Value) -> ToolFuture;

/// Registry of all MCP tools with lookup by canonical name.
pub struct ToolRegistry {
    definitions: Vec<ToolDef>,
    executors: Vec<ToolExecutor>,
    lookup: HashMap<&'static str, usize>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            definitions: Vec::new(),
            executors: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    /// Register a tool type.
    ///
    /// Registration failures are startup-fatal panics. The registry validates
    /// canonical names and aliases before mutating `definitions`, `executors`,
    /// or `lookup`, so failed registration cannot leave a partially registered
    /// tool behind.
    pub fn register<T: McpTool>(&mut self) {
        self.assert_can_register::<T>();

        let index = self.definitions.len();
        self.lookup.reserve(1 + T::ALIASES.len());

        let def = ToolDef {
            name: T::NAME.to_string(),
            description: T::DESCRIPTION.to_string(),
            input_schema: T::input_schema(),
        };

        let executor: ToolExecutor = T::execute;

        self.lookup.insert(T::NAME, index);
        for &alias in T::ALIASES {
            self.lookup.insert(alias, index);
        }

        self.definitions.push(def);
        self.executors.push(executor);
    }

    fn assert_can_register<T: McpTool>(&self) {
        assert!(
            !T::NAME.is_empty(),
            "cannot register MCP tool with an empty canonical name"
        );
        assert!(
            !self.lookup.contains_key(T::NAME),
            "duplicate MCP tool canonical name or alias collision: {}",
            T::NAME
        );

        let mut aliases = HashSet::with_capacity(T::ALIASES.len());
        for &alias in T::ALIASES {
            assert!(
                !alias.is_empty(),
                "cannot register MCP tool {} with an empty alias",
                T::NAME
            );
            assert!(
                alias != T::NAME,
                "cannot register MCP tool {} with an alias equal to its canonical name",
                T::NAME
            );
            assert!(
                aliases.insert(alias),
                "cannot register MCP tool {} with duplicate alias {}",
                T::NAME,
                alias
            );
            assert!(
                !self.lookup.contains_key(alias),
                "duplicate MCP tool alias or canonical-name collision: {}",
                alias
            );
        }
    }

    /// Get all tool definitions for protocol responses.
    pub fn list(&self) -> Vec<ToolDef> {
        self.definitions.clone()
    }

    /// Borrow registered tool definitions without cloning schemas.
    ///
    /// This is intended for server startup paths that serialize or cache the
    /// complete tool list without allowing callers to mutate registry state.
    pub fn definitions(&self) -> &[ToolDef] {
        &self.definitions
    }

    /// Look up and execute a tool by name. Returns None if tool not found.
    pub async fn call(
        &self,
        name: &str,
        id: Option<Value>,
        args: Value,
    ) -> Option<crate::RpcResponse> {
        self.call_with_cancellation(name, id.clone(), args, CancellationToken::new())
            .await
            .and_then(|outcome| outcome.into_rpc_response(id))
    }

    /// Look up and execute a tool by name under a cancellation scope.
    pub async fn call_with_cancellation(
        &self,
        name: &str,
        id: Option<Value>,
        args: Value,
        token: CancellationToken,
    ) -> Option<DispatchOutcome> {
        let executor = self
            .lookup
            .get(name)
            .and_then(|idx| self.executors.get(*idx))?;
        let outcome = CURRENT_CANCEL_TOKEN
            .scope(token.clone(), executor(id, args))
            .await;

        if token.is_cancelled() {
            Some(DispatchOutcome::Cancelled)
        } else {
            Some(DispatchOutcome::Respond(outcome))
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
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
                static INPUT_SCHEMA: std::sync::OnceLock<serde_json::Value> =
                    std::sync::OnceLock::new();

                INPUT_SCHEMA.get_or_init(|| serde_json::json!($schema)).clone()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancellation::current_cancellation_token;
    use serde_json::json;

    #[allow(clippy::unused_async)]
    async fn ok_tool(_id: Option<Value>, _args: Value) -> ToolCallOutcome {
        ToolCallOutcome::ok(json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false
        }))
    }

    #[allow(clippy::unused_async)]
    async fn later_tool(_id: Option<Value>, _args: Value) -> ToolCallOutcome {
        ToolCallOutcome::ok(json!({
            "content": [{"type": "text", "text": "later"}],
            "isError": false
        }))
    }

    #[allow(clippy::unused_async)]
    async fn token_tool(_id: Option<Value>, _args: Value) -> ToolCallOutcome {
        let has_token = current_cancellation_token().is_some();
        ToolCallOutcome::ok(json!({
            "content": [{"type": "text", "text": if has_token { "token" } else { "missing" }}],
            "isError": false
        }))
    }

    async fn wait_for_cancellation_tool(_id: Option<Value>, _args: Value) -> ToolCallOutcome {
        let token =
            current_cancellation_token().expect("task-local cancellation token should exist");
        token.cancelled().await;
        ToolCallOutcome::ok(json!({
            "content": [{"type": "text", "text": "cancelled"}],
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

    define_mcp_tool! {
        TokenTool,
        name: "Token",
        description: "token tool for registry tests",
        schema: {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        handler: token_tool
    }

    define_mcp_tool! {
        WaitForCancellationTool,
        name: "WaitForCancellation",
        description: "waits for cancellation in registry tests",
        schema: {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        handler: wait_for_cancellation_tool
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RegistrySnapshot {
        definitions: Vec<String>,
        executors_len: usize,
        lookup: Vec<(&'static str, usize)>,
    }

    fn registry_snapshot(reg: &ToolRegistry) -> RegistrySnapshot {
        let mut lookup: Vec<_> = reg
            .lookup
            .iter()
            .map(|(name, index)| (*name, *index))
            .collect();
        lookup.sort_unstable();
        RegistrySnapshot {
            definitions: reg
                .definitions
                .iter()
                .map(|definition| definition.name.clone())
                .collect(),
            executors_len: reg.executors.len(),
            lookup,
        }
    }

    fn assert_register_rejected_without_mutating<T: McpTool>(reg: &mut ToolRegistry) {
        let before = registry_snapshot(reg);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reg.register::<T>();
        }));

        assert!(result.is_err());
        assert_eq!(registry_snapshot(reg), before);
    }

    macro_rules! alias_test_tool {
        ($tool:ident, name: $name:expr, aliases: [$($alias:expr),* $(,)?]) => {
            struct $tool;

            impl McpTool for $tool {
                const NAME: &'static str = $name;
                const ALIASES: &'static [&'static str] = &[$($alias),*];
                const DESCRIPTION: &'static str = "alias validation test tool";

                fn input_schema() -> Value {
                    json!({
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    })
                }

                fn execute(
                    id: Option<Value>,
                    args: Value,
                ) -> Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>> {
                    Box::pin(later_tool(id, args))
                }
            }
        };
    }

    #[tokio::test]
    async fn registry_dispatches_by_name() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert!(reg.list().iter().any(|t| t.name == "Dummy"));

        let r1 = reg.call("Dummy", Some(json!(1)), json!({})).await;
        assert!(r1.is_some());
    }

    #[test]
    fn macro_input_schema_returns_owned_values() {
        let mut schema = DummyTool::input_schema();
        schema["additionalProperties"] = json!(true);

        assert_eq!(
            DummyTool::input_schema(),
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn registry_definitions_borrows_registered_tool_definitions() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        let definitions = reg.definitions();

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "Dummy");
        assert_eq!(
            definitions[0].input_schema,
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        );
    }

    #[tokio::test]
    async fn registry_returns_none_for_unknown_tool() {
        let reg = ToolRegistry::new();
        let r = reg.call("nope", Some(json!(1)), json!({})).await;
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn registry_scopes_current_cancellation_token() {
        let mut reg = ToolRegistry::new();
        reg.register::<TokenTool>();

        let token = CancellationToken::new();
        let response = reg
            .call_with_cancellation("Token", Some(json!(1)), json!({}), token)
            .await
            .expect("token tool should resolve")
            .into_rpc_response(Some(json!(1)))
            .expect("token tool should respond");

        assert_eq!(
            response.result.unwrap()["content"][0]["text"],
            json!("token")
        );
    }

    #[tokio::test]
    async fn registry_returns_cancelled_dispatch_outcome_when_token_is_cancelled() {
        let mut reg = ToolRegistry::new();
        reg.register::<WaitForCancellationTool>();

        let token = CancellationToken::new();
        let outcome_task = reg.call_with_cancellation(
            "WaitForCancellation",
            Some(json!(1)),
            json!({}),
            token.clone(),
        );
        tokio::pin!(outcome_task);

        tokio::task::yield_now().await;
        token.cancel();

        let outcome = outcome_task.await.expect("tool should resolve");
        assert!(matches!(outcome, DispatchOutcome::Cancelled));
    }

    struct AliasTool;

    impl McpTool for AliasTool {
        const NAME: &'static str = "Canonical";
        const ALIASES: &'static [&'static str] = &["alias"];
        const DESCRIPTION: &'static str = "tool with alias";

        fn input_schema() -> Value {
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        }

        fn execute(
            id: Option<Value>,
            args: Value,
        ) -> Pin<Box<dyn Future<Output = ToolCallOutcome> + Send>> {
            Box::pin(ok_tool(id, args))
        }
    }

    #[tokio::test]
    async fn registry_dispatches_by_alias_without_listing_alias() {
        let mut reg = ToolRegistry::new();
        reg.register::<AliasTool>();

        let tool_names: Vec<_> = reg.list().into_iter().map(|tool| tool.name).collect();
        assert_eq!(tool_names, vec!["Canonical"]);

        let response = reg.call("alias", Some(json!(1)), json!({})).await;
        assert!(response.is_some());
    }

    define_mcp_tool! {
        AliasNamedTool,
        name: "alias",
        description: "tool whose canonical name conflicts with an earlier alias",
        schema: {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        handler: later_tool
    }

    define_mcp_tool! {
        DuplicateDummyTool,
        name: "Dummy",
        description: "duplicate canonical name",
        schema: {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        },
        handler: later_tool
    }

    alias_test_tool!(
        AliasToExistingCanonicalTool,
        name: "OtherCanonical",
        aliases: ["Dummy"]
    );
    alias_test_tool!(
        AliasCollisionTool,
        name: "OtherAliasCanonical",
        aliases: ["alias"]
    );
    alias_test_tool!(EmptyAliasTool, name: "EmptyAlias", aliases: [""]);
    alias_test_tool!(SelfAliasTool, name: "SelfAlias", aliases: ["SelfAlias"]);
    alias_test_tool!(
        DuplicateAliasWithinTool,
        name: "DuplicateAliasWithin",
        aliases: ["dup", "dup"]
    );

    #[test]
    fn registry_rejects_canonical_name_collisions_before_mutating_state() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert_register_rejected_without_mutating::<DuplicateDummyTool>(&mut reg);
    }

    #[test]
    fn registry_rejects_canonical_name_colliding_with_existing_alias() {
        let mut reg = ToolRegistry::new();
        reg.register::<AliasTool>();

        assert_register_rejected_without_mutating::<AliasNamedTool>(&mut reg);
    }

    #[test]
    fn registry_rejects_alias_colliding_with_existing_canonical() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert_register_rejected_without_mutating::<AliasToExistingCanonicalTool>(&mut reg);
    }

    #[test]
    fn registry_rejects_alias_colliding_with_existing_alias() {
        let mut reg = ToolRegistry::new();
        reg.register::<AliasTool>();

        assert_register_rejected_without_mutating::<AliasCollisionTool>(&mut reg);
    }

    #[test]
    fn registry_rejects_empty_alias_before_mutating_state() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert_register_rejected_without_mutating::<EmptyAliasTool>(&mut reg);
    }

    #[test]
    fn registry_rejects_self_alias_before_mutating_state() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert_register_rejected_without_mutating::<SelfAliasTool>(&mut reg);
    }

    #[test]
    fn registry_rejects_duplicate_alias_within_tool_before_mutating_state() {
        let mut reg = ToolRegistry::new();
        reg.register::<DummyTool>();

        assert_register_rejected_without_mutating::<DuplicateAliasWithinTool>(&mut reg);
    }
}
