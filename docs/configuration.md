# Configuration

This document catalogs environment variables and host prerequisites used by `tools-mcp`.

## Runtime Gates And Framing

| Variable | Values | Lifecycle | Effect |
|---|---|---|---|
| `MCP_ENABLE_GIT` | literal `true` only | Read at server startup | Registers the git tool family, including `GitApply`, `GitHunks`, and `GitStageHunks`. Any other value leaves git tools absent. |
| `MCP_ENABLE_PWSH_TOOL` | literal `true` only | Read at server startup | Registers `Pwsh`. Any other value leaves it absent. |
| `MCP_SKIP_HEADERS` | ASCII case-insensitive `true` | Read once by protocol framing code and cached for process lifetime | Uses raw JSON lines instead of `Content-Length` framing when enabled. |
| `RUST_LOG` | tracing filter | Process startup | Controls tracing verbosity. |

## Build And Version Variables

| Variable | Values | Lifecycle | Effect |
|---|---|---|---|
| `APP_VERSION` | string | Build time | Baked into server initialization responses. |
| `PROTOC` | path to protobuf compiler | Build time | Used by protobuf-dependent semantic-search dependencies when set; otherwise the build uses `protoc` from `PATH`. |
| `ORT_LIB_LOCATION` | path | Build time | Optional ONNX Runtime location for semantic dependencies. |
| `BENCH_GPU_DLL_PATHS` | semicolon-separated paths | Criterion benchmark runtime | Prepends GPU DLL directories to `PATH` for semantic GPU benchmarks only. |

## Semantic Search

| Variable | Values | Effect |
|---|---|---|
| `MCP_SEMANTIC_BACKEND` | absent, empty, `lancedb`, or `qdrant`; backend names are case-insensitive after trimming | Startup registration gate and backend selector. When absent, `SemanticIndex` and `SemanticSearch` are not registered. When present and empty or `lancedb`, calls use LanceDB. When present and `qdrant`, calls use Qdrant and require `QDRANT_URL`. Unsupported present values still register the semantic tools, but semantic calls fail with an unsupported-backend error. |
| `QDRANT_URL` | absolute `http` or `https` URL with host and no path/query/fragment | Required when `MCP_SEMANTIC_BACKEND=qdrant`. If no port is present, the client adds Qdrant gRPC port `6334`. |
| `QDRANT_API_KEY` | secret string | Optional Qdrant API key. Empty or unset means no API key. |

## Search And Cache Tuning

Invalid numeric values fall back to the compiled defaults. Search boolean values accept `1`, `true`, `yes`, `on`, `0`, `false`, `no`, and `off` after trimming and lowercasing; unknown values fall back to the compiled default.

| Variable | Default | Effect |
|---|---:|---|
| `TOOLS_SEARCH_INDEX_MAX_FILE_BYTES` | `1048576` | Max bytes per file considered by the in-memory `Search` index. |
| `TOOLS_SEARCH_INDEX_MAX_TOTAL_BYTES` | `268435456` | Max total indexed file bytes per in-memory search snapshot. |
| `TOOLS_SEARCH_INDEX_MAX_FILES` | `50000` | Max files in one in-memory search snapshot. |
| `TOOLS_SEARCH_MAX_CANDIDATES` | `20000` | Max candidate matches examined by memory search. |
| `TOOLS_SEARCH_MAX_FUZZY_PATTERN_CHARS` | `512` | Max fuzzy-search pattern length for memory search. |
| `TOOLS_SEARCH_MAX_FUZZY_VERIFIED_LINES` | `200000` | Max fuzzy-search lines verified after seed lookup. |
| `TOOLS_SEARCH_MAX_FUZZY_LINE_CHARS` | `16384` | Max line length considered for fuzzy verification. |
| `TOOLS_SEARCH_MAX_SHORT_LITERAL_SCAN_LINES` | `200000` | Max lines scanned for short-literal searches. |
| `TOOLS_SEARCH_REGEX_SIZE_LIMIT_BYTES` | `10485760` | Regex size limit for the memory-search regex builder. |
| `TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE` | `false` | Forces full-scope behavior when ignore-rule state requires it. |
| `TOOLS_SEARCH_INDEX_CACHE_MAX_ENTRIES` | `8`; minimum `1` | Max in-memory search index snapshots retained. |
| `TOOLS_SEARCH_INDEX_CACHE_MAX_BYTES` | `0`; `0` disables byte cap | Max bytes retained by the in-memory search index cache. |
| `TOOLS_SEARCH_INDEX_WARM_ENABLED` | `true` | Enables background warming for common search scopes. |
| `TOOLS_SEARCH_INDEX_WARM_START_DELAY_MS` | `250` | Delay before warm-cache work starts; capped at `60000`. |
| `TOOLS_SEARCH_INDEX_WARM_KEY_DELAY_MS` | `25` | Delay between warm-cache keys; capped at `60000`. |
| `TOOLS_SEARCH_INDEX_WARM_TIMEOUT_MS` | `300000` | Warm-cache build timeout. |
| `TOOLS_SEARCH_INDEX_WARM_MAX_KEYS` | `6` | Max warm-cache keys; clamped to `1..=16`. |
| `TOOLS_SEARCH_INDEX_WARM_GLOBS` | `*.rs,*.md` | Comma- or semicolon-separated warm-cache glob list; `none` removes a glob entry. |
| `TOOLS_SEARCH_INDEX_WARM_GIT_TIMEOUT_MS` | `2000` | Git probe timeout for warm-cache scope detection; clamped to `100..=30000`. |
| `TOOLS_SCOPE_CACHE_MAX_ENTRIES` | `32` | Max recursive scope snapshots retained. |
| `TOOLS_SCOPE_CACHE_MAX_FILES_TOTAL` | `200000` | Max total files retained across recursive scope snapshots. |
| `TOOLS_SCOPE_CACHE_FULL_VALIDATE_INTERVAL` | `32` | Query interval before a recursive scope snapshot gets full validation; `0` validates every cache hit. |
| `TOOLS_DIR_CACHE_MAX_ENTRIES` | `64` | Directory listing cache entry cap. |
| `TOOLS_OUTLINE_CACHE_MAX_ENTRIES` | `256` | Outline AST cache entry cap. |

## WebFetch

| Variable | Values | Effect |
|---|---|---|
| `WEBFETCH_ENABLE_BROWSER_UNSAFE` | literal `true` only | Enables browser rendering after SSRF/robots checks. Any other value keeps browser rendering disabled and falls back to HTTP where the request allows fallback. |
| `WEBFETCH_CACHE_TTL_SECONDS` | unsigned integer; default `86400`; `0` expires immediately | Sets WebFetch cache TTL. Invalid or unreadable values fall back to the default. |
| `WEBFETCH_CACHE_MAX_BYTES` | unsigned integer; default `104857600`; `0` prunes all entries after writes | Sets total WebFetch cache quota. Invalid or unreadable values fall back to the default. |
| `CHROME_PATH` | existing path | First-priority Chrome/Chromium executable override. |
| `CHROMIUM_PATH` | existing path | Second-priority Chrome/Chromium executable override. |
| `CHROME_EXECUTABLE` | existing path | Third-priority Chrome/Chromium executable override. |

`WebFetch` stores cache files under the platform temp directory in `tools-webfetch`. Browser executable discovery then checks common install paths and finally `PATH`.

## Host Tools

The server expects `git`, `ugrep`, `protoc`, Rust, and optionally PowerShell 7+ and Chrome/Chromium to be available as documented in the root README.

## Git Subprocess Environment

When git tools are enabled, child git processes do not inherit repository-authority variables such as `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `GIT_INDEX_FILE`, object-directory/alternates variables, replace-ref variables, pathspec-mode variables, trace variables, dynamic `GIT_CONFIG_*` variables, or SSH/ASKPASS helper variables. The runner also sets `GIT_CONFIG_NOSYSTEM=1`, a null `GIT_CONFIG_GLOBAL`, `GIT_EXTERNAL_DIFF=`, `GIT_ATTR_NOSYSTEM=1`, `GIT_NO_LAZY_FETCH=1`, `GIT_OPTIONAL_LOCKS=0`, and `GIT_NO_REPLACE_OBJECTS=1` for git subprocesses.
