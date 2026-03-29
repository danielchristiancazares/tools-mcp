use crate::application::workspace::handle_edit;
use crate::define_mcp_tool;

define_mcp_tool! {
    EditTool,
    name: "Edit",
    aliases: ["edit"],
    description: "Replace a snippet in a file. Finds old_snippet and replaces with new_snippet, preserving line endings.",
    schema: {
        "type": "object",
        "required": ["path", "old_snippet", "new_snippet"],
        "properties": {
            "path": {"type": "string", "description": "File path to edit"},
            "old_snippet": {"type": "string", "description": "Exact text to find and replace"},
            "new_snippet": {"type": "string", "description": "Replacement text (use LF newlines)"},
            "match_hint": {
                "type": "object",
                "description": "Optional line range hint to help locate the snippet",
                "properties": {
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1}
                }
            }
        }
    },
    handler: handle_edit
}
