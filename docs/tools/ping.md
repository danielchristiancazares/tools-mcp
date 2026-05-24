# SDD: Ping

**Date:** 2026-05-24
**Scope:** Design contract for the `Ping` MCP tool.
**Source:** `tools-mcp-server/src/ping.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `Ping` tool.
2. The JSON-RPC method-level `ping` / `mcp/ping` (which exists at the protocol layer and is dispatched without invoking a tool handler) is a *separate* concept covered in `docs/protocol.md`. Do not conflate the two.

## 3. Scope

### 3.1 What This Document Covers

`Ping` is the MCP tool that returns the literal text `"pong"` so MCP clients can verify the server is up, responsive, and able to dispatch tool calls end-to-end. It is the simplest tool in the registry and has no inputs.

### 3.2 Explicitly Out of Scope

- The JSON-RPC method-level `ping` and `mcp/ping` (handled directly by `mcp_server.rs` without going through the tool dispatcher). See `docs/protocol.md`.
- General connectivity diagnostics or transport health checks beyond a single round trip.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `Ping` |
| Aliases | `ping` |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-server` (registered directly in the composition root, not in a feature crate) |
| Handler function | `handle_ping` (`tools-mcp-server/src/ping.rs:12`) |
| Schema definition | `tools-mcp-server/src/ping.rs:31-38` |
| Registration call | `tools-mcp-server/src/composition.rs:86` |

### 4.2 Invariants

- MUST return `"pong"` in `content[0].text` on every successful call.
- MUST reject any arguments other than an empty object via the `#[serde(deny_unknown_fields)]` request type (`ping.rs:7-9`). Extra fields produce a tool-level error with text `"invalid arguments: ..."`.
- MUST NOT perform I/O, network calls, or filesystem access.
- MUST NOT panic; the handler returns `ToolCallOutcome::ok` unconditionally except when argument parsing fails.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT accept any input parameters (today or in future revisions). Health-probe semantics require that callers may include no parameters and expect a fixed response.
- MUST NOT return content other than the literal string `"pong"`.

## 5. Design Goals

- **Trivial, deterministic, side-effect-free.** A health probe that performs any real work would couple liveness signals to subsystems that may be down for unrelated reasons.
- **Two invocation surfaces for client compatibility.** Both the MCP tool name `Ping` and the alias `ping` work, mirroring the protocol-level `ping` / `mcp/ping` methods.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| _(none)_ | — | — | — | `additionalProperties: false` | Tool accepts no input fields. |

```json
{
  "type": "object",
  "properties": {},
  "required": [],
  "additionalProperties": false
}
```

Schema source: `tools-mcp-server/src/ping.rs:31-38`. Argument validation: `tools-mcp-server/src/ping.rs:13-16`.

### 6.2 Behavior

1. **Parse arguments** — Deserialize `args` into the empty `PingRequest` struct (`ping.rs:13-16`). Extra fields fail deserialization.
2. **Build response** — Construct the fixed MCP success envelope and return it (`ping.rs:17-20`).

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "pong"}],
  "isError": false
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].type` | string | Yes | Always `"text"`. |
| `content[0].text` | string | Yes | Always the literal `"pong"`. |
| `isError` | boolean | Yes | Always `false` on success. |

**Tool-level error (`isError: true`):**

Only produced when argument parsing fails (e.g., the caller passes unknown fields):

```json
{
  "content": [{"type": "text", "text": "invalid arguments: unknown field `foo` ... Unknown fields are not allowed; check argument names against the tool schema."}],
  "isError": true
}
```

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| Caller sends unknown field | `true` | `"invalid arguments: unknown field ..."` |
| Caller sends non-object `arguments` | `true` | `"invalid arguments: invalid type ..."` |

The handler does not surface any error condition beyond argument parsing because it performs no other fallible work.

## 7. Security Considerations

Not applicable. The tool reads no input fields, performs no I/O, and accesses no secrets. The fixed response is constant and reveals nothing beyond server liveness.

## 8. Configuration

Not applicable. The tool reads no environment variables.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-server/src/composition.rs` | 86 |
| Tool name + alias | `tools-mcp-server/src/ping.rs` | 27-28 |
| Schema | `tools-mcp-server/src/ping.rs` | 31-38 |
| Handler | `tools-mcp-server/src/ping.rs` | 12-21 |
| `PingRequest` (deny_unknown_fields) | `tools-mcp-server/src/ping.rs` | 7-9 |

## 10. Examples

### 10.1 Call via canonical tool name

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "Ping",
    "arguments": {}
  }
}
```

### 10.2 Call via alias

The alias `ping` resolves to the same tool (`tools-mcp-server/src/ping.rs:28`):

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "ping",
    "arguments": {}
  }
}
```

### 10.3 Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "pong"}],
    "isError": false
  }
}
```

### 10.4 Argument validation failure

Calling with an unknown field yields a tool-level error:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "mcp/tools/call",
  "params": {
    "name": "Ping",
    "arguments": {"echo": "hi"}
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [{"type": "text", "text": "invalid arguments: unknown field `echo`, expected ... Unknown fields are not allowed; check argument names against the tool schema."}],
    "isError": true
  }
}
```

## 11. Testing

No targeted unit tests live alongside `ping.rs`. End-to-end coverage exercises the tool through the integration harness:

| Test | File | What it covers |
|---|---|---|
| `tools_list_contains_ping` / `tools_call_ping_returns_pong` (or equivalent) | `tools-mcp-server/tests/integration_test.rs` | Verifies that the `Ping` tool appears in `mcp/tools/list` and that `mcp/tools/call { name: "Ping" }` returns `"pong"` with `isError: false`. |

Coverage gap: no test explicitly verifies the `ping` alias resolves correctly, nor that unknown arguments produce the redacted argument-parse error. These are reasonable additions but are not required for the current contract because they are covered transitively by the generic registry-level alias handling and `ToolCallOutcome::parse_args` tests in `tools-mcp-core`.

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Is there a difference between the `Ping` tool and the protocol-level `ping` method? | Yes. `Ping` is invoked via `mcp/tools/call { name: "Ping" }` and goes through the tool dispatcher. The protocol-level `ping` / `mcp/ping` is a JSON-RPC method handled directly by the server with no tool involvement. Both exist for client compatibility. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::parse_args` controls the argument-error message (§6.4). |
| `docs/protocol.md` | Authoritative description of the JSON-RPC method-level `ping`. |
