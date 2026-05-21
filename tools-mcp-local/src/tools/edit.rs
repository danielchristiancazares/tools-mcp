use crate::smart_file_edit::handle_edit;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    EditTool,
    name: "Edit",
    description: "Replace a snippet in a file. Finds old_snippet and replaces with new_snippet, preserving line endings.",
    schema: {
        "type": "object",
        "required": ["path", "old_snippet", "new_snippet"],
        "properties": {
            "path": {"type": "string", "description": "File path to edit"},
            "old_snippet": {"type": "string", "description": "Exact text to find and replace"},
            "new_snippet": {"type": "string", "description": "Replacement text (use LF newlines)"},
            "file_hash": {
                "type": "string",
                "description": "Optional expected current file hash. If provided and stale, the edit is rejected without modifying the file."
            },
            "region_id": {
                "type": "string",
                "description": "Optional caller-supplied region identifier echoed in successful edit metadata."
            },
            "match_hint": {
                "type": "object",
                "description": "Optional line range hint to help locate the snippet",
                "properties": {
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    },
    handler: handle_edit
}
