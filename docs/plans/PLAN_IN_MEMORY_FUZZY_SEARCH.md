# In-Memory Fuzzy Search Blueprint

## One-Sentence Summary

Implement memory-backed `Search.fuzzy` only as an exact, ugrep-compatible bounded-edit subset for fixed-string, case-sensitive searches, using conservative exact-seed candidate generation and authoritative Phase Two fuzzy verification.

## Problem Statement

This is a `feature`: the user wants to know how the in-memory `Search` POC can support fuzzy search without weakening the current public `Search` contract or Hauberk handoff constraints. The current SRD explicitly preserves the existing schema and fallback model (`hauberk-in-memory-search-srd.md:16`, `hauberk-in-memory-search-srd.md:18`, `hauberk-in-memory-search-srd.md:20`, `hauberk-in-memory-search-srd.md:22`) but does not currently approve changing fuzzy behavior (`hauberk-in-memory-search-srd.md:187`, `hauberk-in-memory-search-srd.md:191`).

## Current State

- The public `Search` schema already exposes `fuzzy` as an integer tolerance from 1 through 4 (`tools-mcp-local/src/tools/search.rs:21`, `tools-mcp-local/src/tools/search.rs:23`), and the deserialized request stores it as `Option<u8>` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:53`, `tools-mcp-local/src/tools/handlers/ripgrep.rs:55`).
- The ugrep backend clamps the fuzzy distance to 1 through 4 (`tools-mcp-local/src/tools/handlers/ripgrep.rs:71`, `tools-mcp-local/src/tools/handlers/ripgrep.rs:72`) and passes it directly as `-Z{dist}` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:222`, `tools-mcp-local/src/tools/handlers/ripgrep.rs:224`).
- The handler automatically tries memory search first and delegates to ugrep when the memory backend returns a fallback-allowed error (`tools-mcp-local/src/tools/handlers/ripgrep.rs:196`, `tools-mcp-local/src/tools/handlers/ripgrep.rs:198`, `tools-mcp-local/src/tools/handlers/ripgrep.rs:199`).
- The current memory eligibility check rejects any fuzzy query with `unsupported_fuzzy` (`tools-mcp-local/src/tools/handlers/search_memory.rs:198`, `tools-mcp-local/src/tools/handlers/search_memory.rs:199`, `tools-mcp-local/src/tools/handlers/search_memory.rs:202`, `tools-mcp-local/src/tools/handlers/search_memory.rs:203`).
- The current memory path supports only fixed-string, case-sensitive, non-word, non-glob, non-follow, at-least-three-byte, single-line literals (`tools-mcp-local/src/tools/handlers/search_memory.rs:206`, `tools-mcp-local/src/tools/handlers/search_memory.rs:220`, `tools-mcp-local/src/tools/handlers/search_memory.rs:227`, `tools-mcp-local/src/tools/handlers/search_memory.rs:244`, `tools-mcp-local/src/tools/handlers/search_memory.rs:252`, `tools-mcp-local/src/tools/handlers/search_memory.rs:260`).
- The current memory index is byte-oriented: documents store raw bytes and line ranges (`tools-mcp-local/src/tools/handlers/search_memory.rs:107`, `tools-mcp-local/src/tools/handlers/search_memory.rs:111`, `tools-mcp-local/src/tools/handlers/search_memory.rs:112`), postings are keyed by byte trigrams (`tools-mcp-local/src/tools/handlers/search_memory.rs:121`, `tools-mcp-local/src/tools/handlers/search_memory.rs:125`), and exact candidate generation intersects every trigram in the literal (`tools-mcp-local/src/tools/handlers/search_memory.rs:404`, `tools-mcp-local/src/tools/handlers/search_memory.rs:409`, `tools-mcp-local/src/tools/handlers/search_memory.rs:417`, `tools-mcp-local/src/tools/handlers/search_memory.rs:418`).
- Phase Two is already authoritative in the SRD (`hauberk-in-memory-search-srd.md:239`, `hauberk-in-memory-search-srd.md:240`) and in code, where candidate documents are verified before rendering (`tools-mcp-local/src/tools/handlers/search_memory.rs:150`, `tools-mcp-local/src/tools/handlers/search_memory.rs:152`, `tools-mcp-local/src/tools/handlers/search_memory.rs:153`).
- The current memory renderer emits path-sorted document results, ascending line numbers, context lines, successful `max_results` truncation, and memory diagnostics (`tools-mcp-local/src/tools/handlers/search_memory.rs:419`, `tools-mcp-local/src/tools/handlers/search_memory.rs:423`, `tools-mcp-local/src/tools/handlers/search_memory.rs:455`, `tools-mcp-local/src/tools/handlers/search_memory.rs:473`, `tools-mcp-local/src/tools/handlers/search_memory.rs:474`, `tools-mcp-local/src/tools/handlers/search_memory.rs:178`, `tools-mcp-local/src/tools/handlers/search_memory.rs:195`).
- The README currently documents that fuzzy searches fall back to ugrep (`README.md:31`, `README.md:37`, `README.md:60`, `README.md:644`, `README.md:666`, `README.md:674`).
- Required Hauberk IFA artifacts are not present in this repository checkout: `docs/IFA.md`, `docs/IFA_CONFORMANCE_RULES.md`, `docs/PARALLEL_TOOL_EXECUTION.md`, `SECURITY.md`, `ifa/README.md`, and `ifa/*.toml` have no repo-relative line anchors because they do not exist. The SRD itself warns that this tools-mcp POC is not final Hauberk design authority until Hauberk IFA, security, approval, queueing, and harness invariants are cited and reviewed (`hauberk-in-memory-search-srd.md:10`, `hauberk-in-memory-search-srd.md:12`, `hauberk-in-memory-search-srd.md:13`, `hauberk-in-memory-search-srd.md:14`).

## End State

- Recommended fuzzy semantics:
  - Interpret `fuzzy=N` as the current exposed ugrep `-ZN` behavior for this tool: approximate matching within `N` total insertion, deletion, or substitution errors, not generic ranking or semantic similarity. This follows the current implementation, which passes the integer directly to ugrep as `-Z{dist}` (`tools-mcp-local/src/tools/handlers/ripgrep.rs:222`, `tools-mcp-local/src/tools/handlers/ripgrep.rs:224`).
  - The memory backend MUST NOT expose scores, sort by best score, or invent relevance ranking. It MAY compute the minimum edit distance internally only to decide whether a line is a match.
  - Because the public schema exposes only an integer distance and not ugrep's optional fuzzy operator modifiers, `best`, or best-sort modes (`tools-mcp-local/src/tools/search.rs:23`), the memory implementation MUST model only the integer threshold mode.
- Viable approaches considered:

  | Approach | Strength | Rejection / Use Decision |
  |---|---|---|
  | Full line scan with banded Levenshtein over every selected file | Simple and no false negatives | Rejected as the primary architecture because it discards the trigram index and can turn every fuzzy query into an O(total selected text) scan. MAY be used only as a deliberate fallback inside tiny scopes if separately approved. |
  | Enumerate all edit variants and use exact trigram lookup | Easy to reason about for very short ASCII patterns | Rejected because the variant set grows explosively across Unicode and edit distances 1-4, and deletion/insertion neighborhoods are hard to bound without either missing matches or becoming a full scan. |
  | q-gram count threshold | More selective than seed union for long patterns | Not recommended for the first fuzzy expansion because the exact lower-bound proof is easier to get wrong with substring matching, Unicode scalar edits, and ugrep's first-character behavior. It MAY be a later optimization after parity tests exist. |
  | Partitioned exact seeds plus authoritative verifier | Conservative, simple proof, reuses the current postings index | Recommended final architecture. It intentionally supports a narrower fuzzy subset and falls back whenever the seed proof cannot be established. |

- Recommended final architecture:
  - Add a `SearchPlan` enum inside the memory backend with `ExactLiteral { literal }` and `FuzzyLiteral { pattern, distance, seeds }` variants. The current `eligible_literal` concept becomes plan construction, not a boolean-ish literal extractor.
  - Keep the existing byte-trigram `IndexSnapshot` as the candidate index. Add no public request parameter and no new MCP tool.
  - For `FuzzyLiteral`, parse the pattern as Unicode scalar values because the current ugrep invocation does not request byte mode. The eligible fuzzy subset MUST require all selected candidate files to be valid UTF-8 text and NUL-free; if index construction observes invalid UTF-8 or binary content in scope, the query MUST fall back to ugrep without partial memory results.
  - Candidate generation MUST use the partitioned exact-seed theorem: split the pattern into `distance + 1` contiguous, non-overlapping Unicode-scalar segments. Any substring within Levenshtein distance `distance` can have edits touching at most `distance` segments, so at least one segment remains exact. The memory backend MUST require every segment's UTF-8 encoding to be at least three bytes so each possible untouched segment is searchable through existing trigram postings.
  - For each seed segment, intersect all byte-trigram postings for that seed. The candidate document set is the union of all seed candidate sets. This union is conservative: it may include false positives, but it has no false negatives for the supported fuzzy subset because every valid fuzzy match contains at least one untouched seed and every seed is indexed.
  - Phase Two MUST be authoritative. It MUST run a bounded Unicode-scalar Levenshtein verifier over every candidate line and emit a line only if some line substring is within the requested distance. The verifier MUST implement the current integer ugrep fuzzy mode's insertion, deletion, and substitution costs, including the observed first-pattern-character anchoring rule; if parity cannot be proven for that rule, fuzzy memory eligibility MUST remain disabled.
  - Matching remains line-oriented: a line is emitted once if it contains one or more fuzzy matches. Context rendering, `exit_code`, `count`, `matches`, `truncated`, `timed_out`, and additive backend diagnostics remain shaped like the current memory response (`tools-mcp-local/src/tools/handlers/search_memory.rs:178`, `tools-mcp-local/src/tools/handlers/search_memory.rs:187`, `tools-mcp-local/src/tools/handlers/search_memory.rs:188`, `tools-mcp-local/src/tools/handlers/search_memory.rs:195`).

## Behavior Changes

- Intentional behavior change required by the user's request:
  - Old behavior: every fuzzy query falls back from memory to ugrep because `eligible_literal` rejects `req.fuzzy.is_some()` (`tools-mcp-local/src/tools/handlers/search_memory.rs:198`, `tools-mcp-local/src/tools/handlers/search_memory.rs:205`), and README documents ugrep fallback for fuzzy (`README.md:666`, `README.md:674`).
  - New behavior: eligible fixed-string, case-sensitive fuzzy queries use `backend: "memory"` and only unsupported fuzzy cases fall back to `backend: "ugrep"`.
  - Material concern: public behavior and downstream scripts that inspect `backend`, `fallback_reason`, result order, or truncation.
  - Ratification: update SRD sections that currently disallow fuzzy behavior changes (`hauberk-in-memory-search-srd.md:187`, `hauberk-in-memory-search-srd.md:191`) and tests that currently expect fuzzy fallback (`hauberk-in-memory-search-srd.md:515`, `hauberk-in-memory-search-srd.md:519`).
- Intentional behavior change required by a cited design constraint:
  - Old behavior: fuzzy semantics are delegated entirely to ugrep.
  - New behavior: memory fuzzy supports only a proven subset and delegates all ambiguous cases, preserving the SRD rule that unsupported or ambiguous cases delegate to ugrep (`hauberk-in-memory-search-srd.md:20`, `hauberk-in-memory-search-srd.md:22`, `hauberk-in-memory-search-srd.md:23`).
  - Material concern: correctness and no false negatives.
  - Ratification: parity tests MUST compare memory fuzzy results with ugrep on eligible fixtures, excluding additive metadata.
- Proposed behavior change requiring author approval:
  - Old behavior: ugrep owns fuzzy result traversal and truncation order.
  - New behavior: memory-backed fuzzy uses the existing memory renderer's deterministic path order and ascending line order, then applies `max_results` in that emitted event order (`tools-mcp-local/src/tools/handlers/search_memory.rs:419`, `tools-mcp-local/src/tools/handlers/search_memory.rs:423`, `tools-mcp-local/src/tools/handlers/search_memory.rs:473`, `tools-mcp-local/src/tools/handlers/search_memory.rs:475`).
  - Material concern: users with small `max_results` may see a different prefix than ugrep would have emitted.
  - Ratification: either approve deterministic memory ordering and document it, or make exact ugrep traversal-order parity a hard eligibility requirement and fall back when it cannot be proven.
- Preserved behavior:
  - The request schema MUST NOT change; `Search` remains the public tool name (`hauberk-in-memory-search-srd.md:16`, `hauberk-in-memory-search-srd.md:18`, `hauberk-in-memory-search-srd.md:151`, `hauberk-in-memory-search-srd.md:154`).
  - Non-fuzzy exact fixed-string memory behavior remains unchanged.
  - Regex fuzzy, smart-case fuzzy, case-insensitive fuzzy, word-regexp fuzzy, multiline fuzzy, too-short fuzzy patterns, unpartitionable fuzzy patterns, invalid-UTF-8 fuzzy scopes, binary scopes, incomplete index coverage, stale verification, and resource-limit failures MUST NOT return partial memory success.
  - `max_results` truncation remains a successful truncated result, not a tool error (`hauberk-in-memory-search-srd.md:166`, `hauberk-in-memory-search-srd.md:167`, `hauberk-in-memory-search-srd.md:191`, `hauberk-in-memory-search-srd.md:192`).
  - Memory timeout remains a structured tool error with `timed_out: true`, matching the current memory timeout error path (`tools-mcp-local/src/tools/handlers/search_memory.rs:42`, `tools-mcp-local/src/tools/handlers/search_memory.rs:49`, `tools-mcp-local/src/tools/handlers/search_memory.rs:66`, `tools-mcp-local/src/tools/handlers/search_memory.rs:70`).

## Affected Files

| File | Required change |
|---|---|
| `hauberk-in-memory-search-srd.md` | Update backend eligibility, behavior approvals, required literal extraction, query execution, testing, and docs sections so fuzzy memory support is explicitly approved for the fixed-string subset instead of listed as a required fallback. |
| `tools-mcp-local/src/tools/handlers/search_memory.rs` | Replace `eligible_literal` with plan construction; add fuzzy seed partitioning, seed-union candidate generation, UTF-8 fuzzy eligibility checks, banded Levenshtein verifier, fuzzy-specific fallback reasons, and unit tests. |
| `tools-mcp-local/src/tools/handlers/ripgrep.rs` | Preserve schema parsing and ugrep fallback; add or adjust fallback reason strings only if the memory planner returns new fuzzy-specific reasons. The direct `-Z{dist}` behavior remains the semantic oracle. |
| `tools-mcp-local/src/tools/search.rs` | Keep schema unchanged; update description only if implementation text still implies ugrep-only fuzzy behavior. |
| `README.md` | Replace statements that fuzzy always falls back to ugrep with the exact supported fuzzy subset, fallback rules, and any new additive diagnostics. |
| `tools-mcp-server/tests/integration_test.rs` | Add Search integration tests proving eligible fuzzy uses memory and unsupported fuzzy falls back to ugrep. Existing Search tests remain contract ratchets. |
| `tools-mcp-local/Cargo.toml` | Prefer no dependency change. If an implementation chooses a library verifier, the plan must be amended to name the crate and parity proof; the recommended architecture uses Rust `char` iteration and internal bounded DP. |

## IFA Deltas

- None in this repository because the Hauberk IFA artifacts are absent: `docs/IFA.md`, `docs/IFA_CONFORMANCE_RULES.md`, `ifa/README.md`, and `ifa/*.toml` do not exist in this checkout.
- This absence is itself material: the SRD says the tools-mcp POC MUST NOT be treated as final Hauberk design authority until Hauberk IFA, security, approval, queueing, and harness invariants are cited and reviewed (`hauberk-in-memory-search-srd.md:10`, `hauberk-in-memory-search-srd.md:14`), and the Hauberk follow-up section requires a separate plan for IFA ownership, authority boundaries, queueing/resume interactions, mutation architecture, snapshot lifetime, persistence, and downstream consumers (`hauberk-in-memory-search-srd.md:567`, `hauberk-in-memory-search-srd.md:581`).
- If this design moves into Hauberk, create or update the missing Hauberk IFA artifacts before implementation. They MUST assign ownership for search snapshot authority, fallback authority, freshness proofs, mutation/tombstone semantics, query timeout consequences, and proof-carrying eligibility values.

## UI/Protocol Impact

- No MCP request schema change. The SRD forbids adding a public backend selector in the POC (`hauberk-in-memory-search-srd.md:101`, `hauberk-in-memory-search-srd.md:105`), and the existing schema already contains `fuzzy` (`tools-mcp-local/src/tools/search.rs:23`).
- Response shape remains compatible: `content[0].text`, `isError`, `pattern`, `path`, `exit_code`, `truncated`, `timed_out`, `count`, `matches`, and optional `stderr` remain the baseline (`hauberk-in-memory-search-srd.md:51`, `hauberk-in-memory-search-srd.md:63`).
- Additive metadata remains allowed for memory and fallback responses (`hauberk-in-memory-search-srd.md:179`, `hauberk-in-memory-search-srd.md:184`), so eligible fuzzy memory responses use `backend: "memory"` and unsupported fuzzy responses use `backend: "ugrep"` with a specific `fallback_reason`.
- No UI ranking, score display, or "best match" display is introduced.

## Operation-Graph Impact

None for tools-mcp. This repository implements a stdin/stdout MCP tool server, not Hauberk queueing, approval, continuation, resume, or harness operation-graph execution; those are explicit non-goals in the SRD (`hauberk-in-memory-search-srd.md:87`, `hauberk-in-memory-search-srd.md:95`). If adopted by Hauberk, operation-graph authority and resume semantics must be designed separately as required by the Hauberk follow-up section (`hauberk-in-memory-search-srd.md:569`, `hauberk-in-memory-search-srd.md:581`).

## Test Plan

- Unit tests:
  - `fuzzy_seed_partition_requires_distance_plus_one_searchable_segments`: for distance `d`, planner creates exactly `d + 1` contiguous seeds and rejects patterns whose every segment cannot be at least three UTF-8 bytes.
  - `fuzzy_seed_union_has_no_false_negative_fixture`: construct lines with one edit in each possible segment and prove every matching document is a candidate.
  - `fuzzy_seed_union_intersects_each_seed_trigrams`: verify each seed candidate list is an intersection of that seed's trigrams, and the final candidate set is the union across seeds.
  - `fuzzy_verifier_accepts_insert_delete_substitute_within_distance`: cover insertion, deletion, substitution, exact match, and over-distance rejection.
  - `fuzzy_verifier_emits_line_once`: multiple fuzzy hits on one line produce one match event and shared context rendering.
  - `fuzzy_invalid_utf8_scope_falls_back`: selected invalid UTF-8 text is not answered by memory.
  - `fuzzy_regex_smart_case_word_multiline_unseeded_fall_back`: every unsupported option combination returns a fallback-allowed memory error and no partial results.
  - Existing exact tests for trigram extraction, candidate intersection, context de-duplication, structured errors, regex ineligibility, and freshness remain (`tools-mcp-local/src/tools/handlers/search_memory.rs:693`, `tools-mcp-local/src/tools/handlers/search_memory.rs:775`, `tools-mcp-local/src/tools/handlers/search_memory.rs:777`, `tools-mcp-local/src/tools/handlers/search_memory.rs:821`).
- Integration tests:
  - Eligible fuzzy fixed-string search returns `backend: "memory"`, `isError=false`, matching line text, and no `fallback_reason`.
  - Unsupported fuzzy regex or smart-case search returns `backend: "ugrep"` and a fuzzy-specific `fallback_reason`.
  - For eligible fixtures, compare memory fuzzy output with ugrep output for untruncated result sets, excluding additive metadata. Include exact, insertion, deletion, substitution, no-match, and context cases.
  - `max_results` remains success-shaped and sets `truncated=true` for fuzzy memory searches.
  - Timeout returns `isError=true` and `timed_out=true`, as the SRD requires for memory-backed timeout (`hauberk-in-memory-search-srd.md:166`, `hauberk-in-memory-search-srd.md:168`).
- Documentation tests:
  - README examples and Search documentation must no longer claim that all fuzzy searches fall back to ugrep.

## Out-of-Scope

- Regex fuzzy memory support.
- Case-insensitive or smart-case fuzzy memory support.
- Word-boundary fuzzy memory support.
- Public backend selection.
- Score/ranking output, `best` matching, or best-sort behavior.
- Persistent indexes, file-system watchers, Hauberk queueing, approval, resume, or operation-graph state.
- Replacing ugrep as the fallback oracle.

## Verification

- Baseline for a code implementation: run `just verify` if a `justfile` exists; otherwise run the repository commands from `AGENTS.md`: `cargo fmt --all`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace` (`AGENTS.md:15`, `AGENTS.md:19`).
- Add `just cov` only if coverage tooling exists when the implementation happens; fuzzy matching is coverage-sensitive because it changes matching correctness and fallback boundaries.
- Manual oracle check for implementation PRs: run paired `Search` requests for eligible fuzzy fixtures and direct ugrep `-Z{N}` commands, then verify same matched lines for untruncated cases.

## Risks

- Semantic drift from ugrep: fuzzy details such as Unicode handling and first-character anchoring can be subtle. Mitigation: support only UTF-8 text, keep ugrep as fallback, and require parity tests before enabling memory fuzzy.
- False negatives from candidate filtering: partitioned seeds are safe only if every one of the `distance + 1` segments is searchable. Mitigation: make unpartitionable patterns ineligible; never search only a subset of seeds.
- Truncation prefix drift: deterministic memory path ordering may not match ugrep traversal order. Mitigation: either approve and document memory ordering for memory-backed fuzzy results, or require ugrep-order parity before enabling fuzzy memory under truncation.
- Performance blowups: fuzzy DP can be expensive on many long candidate lines. Mitigation: candidate limit, pattern/line verifier limits, deadline checks, and fallback before partial results when static limits are exceeded.
- Unicode invalidity: current exact memory search can operate on bytes and render lossy text, but ugrep fuzzy defaults to Unicode text. Mitigation: fuzzy memory requires valid UTF-8 selected files or falls back.
- SRD conflict: the current SRD explicitly says fuzzy behavior changes are not approved. Mitigation: update the SRD first and treat fuzzy memory enablement as a separately approved expansion, not as a hidden refactor.
