use super::handlers::handle_search_context;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    SearchContextTool,
    name: "search_context",
    description: "Search file contents and return merged, numbered file windows around each match.",
    schema: {
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "Text or regex to search for. For exact text, identifiers, and error strings, prefer fixed_strings=true."},
            "path": {"type": "string", "description": "File or directory to search (default: current working directory)"},
            "case": {"type": "string", "enum": ["smart", "sensitive", "insensitive"], "default": "smart", "description": "smart = lowercase pattern searches case-insensitively, uppercase stays case-sensitive; use sensitive for exact symbol casing; insensitive ignores case."},
            "fixed_strings": {"type": "boolean", "default": false, "description": "Use for most first calls. Set false only when you intentionally need regex syntax."},
            "word_regexp": {"type": "boolean", "default": false, "description": "Use for whole identifiers/words to avoid substring hits."},
            "glob": {"type": "array", "items": {"type": "string"}, "description": "Limit likely file types or directories, for example **/*.rs or src/**/*.ts."},
            "hidden": {"type": "boolean", "default": false, "description": "Search dotfiles and hidden directories."},
            "follow": {"type": "boolean", "default": false, "description": "Follow symlinks"},
            "no_ignore": {"type": "boolean", "default": false, "description": "Bypass .gitignore/.ignore filtering when expected files are excluded; increases scope and cost."},
            "context_lines": {"type": "integer", "minimum": 0, "maximum": 50, "default": 3, "description": "Number of lines before and after each match to include in returned file windows."},
            "max_matches": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20, "description": "Maximum match lines to expand into context windows."},
            "max_results": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Alias for max_matches; max_matches wins when both are present."},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 10000, "description": "Overall search budget in milliseconds; raise only for intentionally wide searches."},
            "fuzzy": {"type": "integer", "minimum": 1, "maximum": 4, "description": "Use only when spelling is approximate or uncertain. Tolerance is 1-4 edits."}
        },
        "required": ["pattern"],
        "additionalProperties": false
    },
    handler: handle_search_context
}
