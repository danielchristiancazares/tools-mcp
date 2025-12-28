use crate::define_mcp_tool;
use crate::read_file::handle_read_file;

define_mcp_tool! {
    ReadTool,
    name: "Read",
    aliases: ["read", "ReadFile", "read_file", "read-file"],
    description: "Read file contents with optional line range",
    schema: {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to read"
            },
            "start_line": {
                "type": "integer",
                "description": "First line to read (1-indexed)"
            },
            "end_line": {
                "type": "integer",
                "description": "Last line to read (inclusive)"
            },
            "show_line_numbers": {
                "type": "boolean",
                "description": "Include line numbers in output"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_read_file
}
