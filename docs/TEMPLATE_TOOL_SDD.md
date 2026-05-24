# SDD: [Tool Name]

**Date:** YYYY-MM-DD
**Scope:** Design contract for the `[ToolName]` MCP tool.
**Source:** `[path/to/tool.rs]`

---

## Template Use Rules

> Remove this section before adopting the document.

1. This template is for **shipped tool design contracts** — not change proposals. Do not add Implementation Plan slices or "Why baseline is insufficient" sections.
2. Keep all top-level sections in order. Omit a section only when it genuinely does not apply; write `Not applicable.` rather than leaving it blank.
3. Ground every behavioral claim in a code anchor (`file:line`). Unanchored claims are not normative.
4. One SDD covers exactly one MCP tool. Tools sharing infrastructure (e.g., Search + search_context) MAY reference each other but MUST each have their own SDD.
5. When a behavior is gated (env var, feature flag, registration gate), state the gate in §4 and §6.1. Never describe gated behavior as unconditional.
6. Keep three things distinct: directly observed facts (code), normative requirements (MUST/SHOULD/MAY), and open questions. Do not mix them.
7. Do not leave `TODO`, `TBD`, or empty tables in an adopted document.

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `[ToolName]` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, this document is wrong and MUST be corrected — not the implementation, unless the implementation is itself the defect.

## 3. Scope

### 3.1 What This Document Covers

[One paragraph. Name the tool, its MCP tool name, the crate that owns it, and the handler function. State what the tool does at a high level.]

### 3.2 Explicitly Out of Scope

- [Name nearby concerns this SDD intentionally does not cover — e.g., other tools that share infrastructure, server-level routing, MCP framing.]

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `[ToolName]` |
| Aliases | `[alias1]`, `[alias2]` — or "None" |
| Registration gate | `[ENV_VAR=value]` required — or "Always registered" |
| Owning crate | `[tools-mcp-xyz]` |
| Handler function | `[fn_name]` (`[path/to/handler.rs]`) |
| Schema definition | `[path/to/tool_def.rs]` |

### 4.2 Invariants

Behavioral guarantees that MUST hold regardless of input:

- [Invariant 1. Example: MUST validate the URL for SSRF before any cache lookup.]
- [Invariant 2. Example: MUST return `isError: true` on all handler-level failures; MUST NOT panic.]
- [Add only invariants this tool actually maintains.]

### 4.3 Explicitly Forbidden Shapes

Behaviors this tool MUST NOT exhibit:

- [Example: MUST NOT execute fetched content as commands.]
- [Example: MUST NOT write to paths outside the workspace root.]
- [Example: MUST NOT bypass the registration gate to register itself.]

## 5. Design Goals

[2–4 bullet points. Why was this tool designed the way it is? What tradeoffs were made? This is design rationale, not a roadmap.]

- [Example: Prefer HTTP-first rendering to minimize browser process overhead and SSRF attack surface.]
- [Example: Cache by URL + rendering method to avoid a cached HTTP shell poisoning a future browser-rendered result.]

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `[field]` | `[string\|integer\|boolean\|string[]]` | Yes / No | — / `[value]` | [min/max/enum] | [Description] |

> Schema source: `[path/to/tool_def.rs:NN]`

### 6.2 Behavior

Ordered execution steps. Each step MUST be anchored to a code location.

1. **[Step name]** — [Description.] (`[file:line]`)
2. **[Step name]** — [Description.] (`[file:line]`)
3. **[Step name]** — [Description.] (`[file:line]`)

[Include branching logic, fallback paths, and degraded-mode behavior explicitly. Never describe a conditional path as unconditional.]

### 6.3 Response Schema

**Success (`isError: false`):**

```json
{
  "content": [{"type": "text", "text": "..."}],
  "isError": false,
  "[field]": "[value — describe type and semantics]"
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | [What it contains] |
| `isError` | boolean | Yes | Always `false` on success |
| `[other_field]` | [type] | Yes / No | [Description] |

**Tool-level error (`isError: true`):**

```json
{
  "content": [{"type": "text", "text": "error message"}],
  "isError": true
}
```

Tool-level errors use `ToolCallOutcome::err` or `ToolCallOutcome::err_with` (`tools-mcp-core/src/tool_outcome.rs:35,43`). The handler MUST NOT panic; all failure paths MUST return a `ToolCallOutcome`.

### 6.4 Error Catalog

| Condition | `isError` | Text content pattern |
|---|---|---|
| [Error condition, e.g., SSRF blocked] | `true` | `"[error prefix]..."` |
| [Error condition, e.g., missing required field] | `true` | `"invalid arguments: ..."` |
| [Add all distinct error conditions] | | |

## 7. Security Considerations

[Only include what is relevant to this tool. If none apply, write `Not applicable.`]

- **[Concern, e.g., SSRF]** — [How the tool defends against it. Reference `file:line`.]
- **[Concern, e.g., path traversal]** — [Mitigation.]
- **[Concern, e.g., prompt injection]** — [Trust boundary and framing guidance.]

## 8. Configuration

Environment variables read by this tool at runtime. Variables listed here MUST appear in `docs/configuration.md`.

| Variable | Default | Description |
|---|---|---|
| `[ENV_VAR]` | `[default]` | [What it controls] |

If this tool reads no env vars beyond the registration gate, write `Not applicable.`

## 9. Code Anchors

Primary source locations for audit and review. Verify behavioral claims above against these files.

| Claim | File | Line(s) |
|---|---|---|
| Tool name / alias registration | `[file]` | `[NN]` |
| Input schema | `[file]` | `[NN]` |
| Handler entry point | `[file]` | `[NN]` |
| [Key behavioral invariant] | `[file]` | `[NN]` |
| [Security check] | `[file]` | `[NN]` |

## 10. Examples

### Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "[ToolName]",
    "arguments": {
      "[required_field]": "[value]"
    }
  }
}
```

### Success response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "[example output]"}],
    "isError": false
  }
}
```

### [Optional: named scenario demonstrating a key behavior]

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "[ToolName]",
    "arguments": {
      "[field]": "[value that exercises important behavior]"
    }
  }
}
```

## 11. Testing

Tests that encode the current behavioral contract. A regression against any of these indicates a breaking change.

| Test | File | What it covers |
|---|---|---|
| `[test_name]` | `[path/to/test.rs:NN]` | [What behavioral invariant this test locks in] |

If no targeted tests exist for this tool, write `Not applicable.` and note it as a coverage gap.

## 12. Open Questions

[List only genuine blockers or unresolved design ambiguities. If none, write `None.`]

1. [Question]

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | [Question] | [Resolution] |

If none, write `None.`

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome` shape and error constructors (§6.3) |
| `tools-mcp-server/src/composition.rs` | Registration call site (§4.1) |
| `[other relevant file]` | [Why referenced] |
