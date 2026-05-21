use super::handlers::handle_read_file;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    ReadTool,
    name: "Read",
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
                "description": "Include line numbers in output (default: false)"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_read_file
}
