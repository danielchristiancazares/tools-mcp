use crate::define_mcp_tool;
use crate::smart_file_edit::handle_smart_file_edit;

define_mcp_tool! {
    EditTool,
    name: "Edit",
    aliases: ["edit", "smart_file_edit", "SmartFileEdit"],
    description: "Apply surgical text replacements to a file, preserving line endings",
    schema: {
        "type": "object",
        "required": ["action", "path"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["get_region", "apply_snippet_edit", "apply_unified_diff"],
                "description": "Operation to perform"
            },
            "path": {"type": "string", "description": "Filesystem path to inspect or edit"},
            "start_line": {"type": "integer", "minimum": 1, "description": "Start line for get_region"},
            "end_line": {"type": "integer", "minimum": 1, "description": "End line for get_region"},
            "old_snippet": {"type": "string", "description": "Existing canonical snippet to replace"},
            "new_snippet": {"type": "string", "description": "Replacement snippet using LF newlines"},
            "diff": {"type": "string", "description": "Unified diff to apply (for apply_unified_diff action)"},
            "file_hash": {"type": "string", "description": "sha256 hash returned by get_region to detect stale files"},
            "region_id": {"type": "string", "description": "Region identifier returned by get_region"},
            "match_hint": {
                "type": "object",
                "properties": {
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": true
    },
    handler: handle_smart_file_edit
}
