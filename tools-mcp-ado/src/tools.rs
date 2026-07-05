use crate::work_items::handle_ado_work_items;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    AdoWorkItemsTool,
    name: "AdoWorkItems",
    description: "Look up Azure DevOps (ADO) work items by ID, keyword, or assignee. Requires explicit organization and project arguments; authenticates with a short-lived Azure CLI token (az login) so no PAT is stored.",
    schema: {
        "type": "object",
        "properties": {
            "organization": {
                "type": "string",
                "minLength": 1,
                "description": "Azure DevOps organization slug, dev.azure.com/{org} URL, or {org}.visualstudio.com URL. Required."
            },
            "project": {
                "type": "string",
                "minLength": 1,
                "description": "Azure DevOps project name. Required."
            },
            "id": {
                "type": "integer",
                "minimum": 1,
                "description": "Exact Azure DevOps work item ID to fetch."
            },
            "number": {
                "type": "integer",
                "minimum": 1,
                "description": "Alias for id."
            },
            "keyword": {
                "type": "string",
                "minLength": 1,
                "description": "Keyword or phrase to search in title, description, and tags."
            },
            "assigned_to": {
                "type": "string",
                "minLength": 1,
                "description": "Display name or email/unique name to match in System.AssignedTo."
            },
            "state": {
                "type": "string",
                "minLength": 1,
                "description": "Optional state filter, for example Active, New, Resolved, or Closed."
            },
            "work_item_type": {
                "type": "string",
                "minLength": 1,
                "description": "Optional work item type filter, for example Bug, Task, User Story, or Feature."
            },
            "top": {
                "type": "integer",
                "minimum": 1,
                "maximum": 100,
                "default": 20,
                "description": "Maximum search results to return. Applies only to keyword/assignee searches."
            },
            "include_description": {
                "type": "boolean",
                "default": false,
                "description": "Include the raw System.Description field, truncated to a bounded size."
            },
            "timeout_ms": {
                "type": "integer",
                "minimum": 100,
                "maximum": 60000,
                "default": 15000,
                "description": "HTTP request timeout in milliseconds."
            },
            "resource": {
                "type": "string",
                "minLength": 1,
                "description": "Azure CLI access-token audience for `az account get-access-token --resource`. Must be an Azure application ID (GUID) or an https URL. Defaults to the Azure DevOps application ID."
            }
        },
        "anyOf": [
            {"required": ["id"]},
            {"required": ["number"]},
            {"required": ["keyword"]},
            {"required": ["assigned_to"]}
        ],
        "required": ["organization", "project"],
        "additionalProperties": false
    },
    handler: handle_ado_work_items
}
