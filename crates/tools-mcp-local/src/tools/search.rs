use super::handlers::handle_ripgrep;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    SearchTool,
    name: "Search",
    description: "Search file contents using ugrep with regex support",
    schema: {
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "Search pattern (regex by default)"},
            "path": {"type": "string", "description": "File or directory to search (default: current working directory)"},
            "case": {"type": "string", "enum": ["smart", "sensitive", "insensitive"], "default": "smart", "description": "Case handling mode"},
            "fixed_strings": {"type": "boolean", "default": false, "description": "Treat pattern as a literal string (-F)"},
            "word_regexp": {"type": "boolean", "default": false, "description": "Match on word boundaries only (-w)"},
            "glob": {"type": "array", "items": {"type": "string"}, "description": "Optional glob filters"},
            "hidden": {"type": "boolean", "default": false, "description": "Search hidden files/directories"},
            "follow": {"type": "boolean", "default": false, "description": "Follow symlinks"},
            "no_ignore": {"type": "boolean", "default": false, "description": "Do not respect ignore files like .gitignore"},
            "context": {"type": "integer", "minimum": 0, "default": 0, "description": "Lines of context on both sides"},
            "max_results": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 200, "description": "Maximum match/context events to return"},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 20000, "description": "Overall timeout in milliseconds"},
            "fuzzy": {"type": "integer", "minimum": 1, "maximum": 4, "description": "Fuzzy match tolerance (1-4 edits)"}
        },
        "required": ["pattern"],
        "additionalProperties": false
    },
    handler: handle_ripgrep
}
