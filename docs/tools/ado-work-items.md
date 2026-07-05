# SDD: AdoWorkItems

**Date:** 2026-06-24
**Scope:** Design contract for the `AdoWorkItems` MCP tool.
**Source:** `tools-mcp-ado/src/work_items.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Scope

`AdoWorkItems` is a read-only Azure DevOps Services tool for looking up work items by exact ID/number, by keyword, or by assignee display name/email. It is owned by the `tools-mcp-ado` crate and is registered by `tools_mcp_ado::register_tools` from the server composition root. It obtains a short-lived Azure DevOps access token from Azure CLI (`az login`) instead of accepting or storing a PAT.

On-prem Azure DevOps Server and arbitrary Azure DevOps base URLs are out of scope. The tool always targets fixed Azure DevOps Services hosts derived from a validated organization slug.

## 3. Design Contract

### 3.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `AdoWorkItems` |
| Aliases | None |
| Registration gate | Always registered |
| Owning crate | `tools-mcp-ado` |
| Handler function | `handle_ado_work_items` |
| Schema definition | `tools-mcp-ado/src/tools.rs` |

### 3.2 Invariants

- **No ambient configuration** — Organization, project, and the token audience MUST come from explicit tool arguments. The tool MUST NOT read organization, project, tokens, or the audience from process environment variables, which would be an injection/attack surface.
- **No token arguments** — Authentication tokens (PATs or access tokens) MUST NOT be accepted through MCP tool arguments. The handler obtains a Bearer token only from Azure CLI.
- **Azure CLI auth** — The handler MUST authenticate by invoking only the fixed no-shell argument vector `az account get-access-token --resource <resource> --query accessToken -o tsv` and MUST send the result as `Authorization: Bearer <token>`. `<resource>` defaults to the Azure DevOps application ID `499b84ac-1321-427f-aa17-267ca6975798`.
- **Resource validation** — The optional `resource` argument MUST be a canonical GUID or an `https` URL; any other value MUST be rejected before the Azure CLI is invoked so a caller-influenced value cannot reach the command.
- **Fixed host construction** — API URLs MUST be constructed under `https://dev.azure.com/{organization}/{project}/...`; callers MUST NOT be able to supply an arbitrary host.
- **Organization validation** — `organization` MUST normalize to an Azure DevOps Services organization slug containing only ASCII letters, digits, and hyphens, starting and ending with an alphanumeric character.
- **Project path safety** — `project` MUST NOT be empty and MUST NOT contain `/` or `\`.
- **Lookup selector required** — Calls MUST provide exactly one exact selector (`id`/`number`) or at least one search selector (`keyword` and/or `assigned_to`). Exact ID lookup MUST NOT be combined with search/filter fields.
- **Bounded search** — `top` MUST be between 1 and 100. `timeout_ms` MUST be between 100 and 60000.
- **Bounded descriptions** — `System.Description` MUST be omitted unless `include_description=true`; when included it MUST be truncated to a bounded size and report `description_truncated=true` when truncated.

## 4. Tool Specification

### 4.1 Authentication

Authentication uses Azure CLI only; there are no authentication environment variables. The signed-in Azure CLI account MUST have Azure DevOps Work Items read access for the target organization/project. The tool runs:

```bash
az account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 --query accessToken -o tsv
```

The `--resource` audience is the default Azure DevOps application ID and MAY be overridden per call with the `resource` argument (GUID or `https` URL). The command is invoked with a fixed argument vector (no shell), so arguments cannot inject additional flags or commands.

### 4.2 Input Schema

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `organization` | string | Yes | — | Org slug, `https://dev.azure.com/{org}` URL, or `https://{org}.visualstudio.com` URL. |
| `project` | string | Yes | — | Azure DevOps project name. |
| `id` | integer | One of `id`, `number`, `keyword`, `assigned_to` | — | Exact work item ID. |
| `number` | integer | Same as `id` | — | Alias for `id`. |
| `keyword` | string | Search selector | — | Keyword or phrase searched in title, description, and tags via WIQL. |
| `assigned_to` | string | Search selector | — | Exact `System.AssignedTo` display name/email/unique name. |
| `state` | string | No | — | Optional search filter. |
| `work_item_type` | string | No | — | Optional search filter. |
| `top` | integer | No | `20` | Search result limit, 1-100. |
| `include_description` | boolean | No | `false` | Include bounded raw `System.Description`. |
| `timeout_ms` | integer | No | `15000` | HTTP timeout, 100-60000 ms. |
| `resource` | string | No | Azure DevOps app ID | Azure CLI token audience (GUID or `https` URL). |

### 4.3 Behavior

1. Resolve and validate `organization` and `project` from arguments (no environment fallback).
2. Resolve the token audience: the `resource` argument when valid, otherwise the default Azure DevOps application ID.
3. Validate lookup shape.
4. Obtain a Bearer token from Azure CLI for the resolved audience.
5. For `id`/`number`, call the Work Item Tracking `workitems/{id}` API.
6. For search, issue a WIQL query scoped to `System.TeamProject`, combining `keyword`, `assigned_to`, `state`, and `work_item_type` filters, then hydrate returned IDs with `workitemsbatch`.
7. Return a text summary plus structured top-level fields.

### 4.4 Response Shape

Success responses set `isError=false` and include:

| Field | Type | Description |
|---|---|---|
| `content[0].text` | string | Human-readable summary. |
| `organization` | string | Normalized organization slug. |
| `project` | string | Project used for the request. |
| `resource` | string | Azure CLI token audience used. |
| `count` | integer | Number of returned work items. |
| `lookup` | object | Echo of normalized lookup inputs. |
| `work_items` | array | Work item summaries with ID, URLs, title, state, type, identity fields, dates, paths, tags, and optional description. |

Tool errors set `isError=true` and include `error_type` plus `remediation`. HTTP status failures also include `status`, `reason`, and a bounded `details` body when Azure DevOps returns one.
