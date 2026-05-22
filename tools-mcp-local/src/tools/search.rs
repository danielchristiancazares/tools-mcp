use super::handlers::handle_search;
use tools_mcp_core::define_mcp_tool;

define_mcp_tool! {
    SearchTool,
    name: "Search",
    description: "Search file contents for exact text, identifiers, error strings, or regex across a repo; narrow with path/glob, use context for nearby lines, word_regexp for whole identifiers, fuzzy when spelling is uncertain, and hidden/no_ignore only when repo filtering may hide results.",
    schema: {
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "Text or regex to search for. For exact text, identifiers, and error strings, prefer fixed_strings=true."},
            "path": {"type": "string", "description": "File or directory to search (default: current working directory)"},
            "case": {"type": "string", "enum": ["smart", "sensitive", "insensitive"], "default": "smart", "description": "smart = lowercase pattern searches case-insensitively, uppercase stays case-sensitive; use sensitive for exact symbol casing; insensitive ignores case."},
            "fixed_strings": {"type": "boolean", "default": false, "description": "Use for most first calls. Set false only when you intentionally need regex syntax."},
            "word_regexp": {"type": "boolean", "default": false, "description": "Use for whole identifiers/words to avoid substring hits."},
            "glob": {"type": "array", "items": {"type": "string"}, "description": "Limit likely file types or directories, for example **/*.rs or src/**/*.ts, to reduce noise and tokens."},
            "hidden": {"type": "boolean", "default": false, "description": "Search dotfiles and hidden directories; usually only needed for config or tooling files."},
            "follow": {"type": "boolean", "default": false, "description": "Follow symlinks"},
            "no_ignore": {"type": "boolean", "default": false, "description": "Bypass .gitignore/.ignore filtering when expected files are excluded; increases scope and cost."},
            "context": {"type": "integer", "minimum": 0, "default": 0, "description": "Use 0 for broad discovery; add 1-3 only when surrounding lines matter."},
            "max_results": {"type": "integer", "minimum": 1, "maximum": 10000, "default": 100, "description": "Maximum returned events; lower values are better for first-pass discovery."},
            "timeout_ms": {"type": "integer", "minimum": 100, "default": 10000, "description": "Overall search budget in milliseconds; raise only for intentionally wide searches."},
            "fuzzy": {"type": "integer", "minimum": 1, "maximum": 4, "description": "Use only when spelling is approximate or uncertain; broader and more expensive than exact search. Tolerance is 1-4 edits."}
        },
        "required": ["pattern"],
        "additionalProperties": false
    },
    handler: handle_search
}
