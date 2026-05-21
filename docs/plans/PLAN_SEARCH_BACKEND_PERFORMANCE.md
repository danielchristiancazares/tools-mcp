# Search Backend Performance Blueprint

## One-Sentence Summary

Implement the search memory-backend performance series as three behavior-preserving PRs: index-build hot-loop cleanup, ignore-rule-aware targeted freshness, and max-results-aware verification/rendering.

## Problem Statement

The `Search` memory backend is now the default fast path for eligible literal, seeded-regex, and narrow fuzzy queries, but it still spends avoidable CPU on cold indexing, warm-cache freshness, and per-document verification. The implementation must reduce that work without changing the public `Search` schema, ugrep fallback contract, memory-vs-ugrep parity, freshness guarantees, timeout behavior, or `max_results` rendering semantics.

## Current State

- The public `Search` tool schema keeps `fixed_strings=false`, `no_ignore=false`, `max_results=100`, and `timeout_ms=10000` defaults in the MCP tool definition at `tools-mcp-local/src/tools/search.rs:14`, `tools-mcp-local/src/tools/search.rs:19`, `tools-mcp-local/src/tools/search.rs:21`, and `tools-mcp-local/src/tools/search.rs:22`.
- Normalization currently keeps `fixed_strings=false`, `hidden=false`, `follow=false`, `no_ignore=false`, clamps `max_results` to `1..=10000`, and clamps `timeout_ms` to `100..=300000` at `tools-mcp-local/src/tools/handlers/search_contract.rs:165` through `tools-mcp-local/src/tools/handlers/search_contract.rs:180`.
- The design authority for this repository says the `Search` tool name and schema remain stable, unsupported or ambiguous memory cases delegate to `ugrep`, memory responses only add metadata, stale success is forbidden, and `max_results` truncation remains success-shaped at `docs/hauberk-in-memory-search-srd.md:16`, `docs/hauberk-in-memory-search-srd.md:64`, `docs/hauberk-in-memory-search-srd.md:102`, `docs/hauberk-in-memory-search-srd.md:164`, `docs/hauberk-in-memory-search-srd.md:179`, `docs/hauberk-in-memory-search-srd.md:183`, and `docs/hauberk-in-memory-search-srd.md:616`.
- The SRD requires conservative trigram candidate filtering, authoritative Phase Two verification, exact file-selection parity, query-bound freshness, and deterministic match/context rendering at `docs/hauberk-in-memory-search-srd.md:317`, `docs/hauberk-in-memory-search-srd.md:324`, `docs/hauberk-in-memory-search-srd.md:367`, `docs/hauberk-in-memory-search-srd.md:463`, `docs/hauberk-in-memory-search-srd.md:489`, and `docs/hauberk-in-memory-search-srd.md:511`.
- `IndexSnapshot` currently stores documents, a directory-only `ScopeFingerprint`, sensitive postings, ASCII-folded postings, indexed bytes, and the UTF-8 scope flag at `tools-mcp-local/src/tools/handlers/search_memory.rs:377` through `tools-mcp-local/src/tools/handlers/search_memory.rs:385`.
- `FileStamp` stores `len`, `modified`, `change_marker`, and a SHA-256 `hash` at `tools-mcp-local/src/tools/handlers/search_memory.rs:189` through `tools-mcp-local/src/tools/handlers/search_memory.rs:195`; every indexed file is hashed during build by `file_stamp_from_parts_with_deadline` at `tools-mcp-local/src/tools/handlers/search_memory.rs:5234` through `tools-mcp-local/src/tools/handlers/search_memory.rs:5244`.
- Cold index build reads file content, rejects binary or invalid UTF-8 when required, hashes content, computes line ranges, sorts documents, then computes sensitive trigrams and ASCII-folded trigrams in separate passes at `tools-mcp-local/src/tools/handlers/search_memory.rs:2622` through `tools-mcp-local/src/tools/handlers/search_memory.rs:2676` and `tools-mcp-local/src/tools/handlers/search_memory.rs:2682` through `tools-mcp-local/src/tools/handlers/search_memory.rs:2705`.
- `literal_trigrams_with_deadline` checks the deadline on every 3-byte window, `ascii_folded_bytes_with_deadline` checks on every byte, `unique_trigrams_with_deadline` builds a `Vec` and then recollects into a `HashSet`, `intersect_postings` checks on every comparison, and `matching_line_indexes` checks on every line at `tools-mcp-local/src/tools/handlers/search_memory.rs:3932`, `tools-mcp-local/src/tools/handlers/search_memory.rs:3948`, `tools-mcp-local/src/tools/handlers/search_memory.rs:4269`, `tools-mcp-local/src/tools/handlers/search_memory.rs:4278`, and `tools-mcp-local/src/tools/handlers/search_memory.rs:4318`.
- The current deadline primitive calls `Instant::now()` directly for every `check_deadline` call at `tools-mcp-local/src/tools/handlers/search_memory.rs:5310` through `tools-mcp-local/src/tools/handlers/search_memory.rs:5315`.
- Warm validation currently forces a full-scope scan whenever `no_ignore=false`, which is the default, via `SnapshotValidation::validate` at `tools-mcp-local/src/tools/handlers/search_memory.rs:3683` through `tools-mcp-local/src/tools/handlers/search_memory.rs:3703`.
- The index manager already forces a full validation every 32 targeted validations through `TARGETED_FRESHNESS_FULL_SCAN_INTERVAL_QUERIES` and `full_validation_due` at `tools-mcp-local/src/tools/handlers/search_memory.rs:48`, `tools-mcp-local/src/tools/handlers/search_memory.rs:504`, and `tools-mcp-local/src/tools/handlers/search_memory.rs:829`.
- Full-scope freshness calls `discover_files`, compares expected and observed path sets, then metadata-validates every indexed document at `tools-mcp-local/src/tools/handlers/search_memory.rs:3875` through `tools-mcp-local/src/tools/handlers/search_memory.rs:3917`.
- File discovery for memory search routes through `FileSelector::discover_memory_scope`; directory roots use `repo_scope_cache`, while single-file roots still use `WalkBuilder` directly at `tools-mcp-local/src/tools/handlers/search_file_selection.rs:142` through `tools-mcp-local/src/tools/handlers/search_file_selection.rs:161` and `tools-mcp-local/src/tools/handlers/search_file_selection.rs:223` through `tools-mcp-local/src/tools/handlers/search_file_selection.rs:276`.
- Both memory discovery and shared scope-cache discovery configure `WalkBuilder` with `.ignore(!no_ignore)`, `.git_ignore(!no_ignore)`, `.git_global(!no_ignore)`, and `.git_exclude(!no_ignore)` at `tools-mcp-local/src/tools/handlers/search_file_selection.rs:370` through `tools-mcp-local/src/tools/handlers/search_file_selection.rs:380` and `tools-mcp-local/src/tools/scope_cache.rs:568` through `tools-mcp-local/src/tools/scope_cache.rs:576`.
- `repo_scope_cache` currently detects staleness from directory metadata fingerprints and a periodic full-validate interval, but it does not fingerprint ignore-control files at `tools-mcp-local/src/tools/scope_cache.rs:145` through `tools-mcp-local/src/tools/scope_cache.rs:205` and `tools-mcp-local/src/tools/scope_cache.rs:686` through `tools-mcp-local/src/tools/scope_cache.rs:710`.
- The locked `ignore` crate is `0.4.25` at `Cargo.lock:1452`; its local source documents that default walking respects `.ignore`, `.gitignore`, `.git/info/exclude`, and global gitignore files, and it exposes `ignore::gitignore::gitconfig_excludes_path()` for the global gitignore path.
- `verify_and_render` computes all matching line indexes for each candidate document before rendering, and only stops at the document boundary after `push_rendered_events` reports truncation at `tools-mcp-local/src/tools/handlers/search_memory.rs:3561` through `tools-mcp-local/src/tools/handlers/search_memory.rs:3620`.
- `push_rendered_events` and its helpers merge context intervals, preserve event order, and stop when `events.len() >= max_results` at `tools-mcp-local/src/tools/handlers/search_memory.rs:4843` through `tools-mcp-local/src/tools/handlers/search_memory.rs:4969`.
- Memory responses add telemetry fields including `backend`, `index_cache`, `candidate_count`, `verified_line_count`, `freshness_scope`, and `freshness_full_scan_reason` at `tools-mcp-local/src/tools/handlers/search_memory.rs:1582` through `tools-mcp-local/src/tools/handlers/search_memory.rs:1723`.
- Rendering currently constructs `RenderedSearchEvent` at least three times per event across payload building, text-capacity calculation, and text rendering at `tools-mcp-local/src/tools/handlers/search_contract.rs:375`, `tools-mcp-local/src/tools/handlers/search_contract.rs:419`, and `tools-mcp-local/src/tools/handlers/search_contract.rs:427`.
- `ripgrep.rs` also uses `render_search_text` and `build_search_payload`, so any render API refactor must preserve the existing wrapper path for ugrep fallback at `tools-mcp-local/src/tools/handlers/ripgrep.rs:517` through `tools-mcp-local/src/tools/handlers/ripgrep.rs:535`.
- Existing parity tests normalize memory and forced-ugrep output while comparing behavior fields and matches at `tools-mcp-local/src/tools/handlers/search_parity.rs:127` through `tools-mcp-local/src/tools/handlers/search_parity.rs:242`.
- Existing server integration tests assert memory backend activation for fixed strings, default literal patterns, seeded regexes, fuzzy fixed strings, and glob-filtered fixed strings, and assert ugrep fallback for unseeded regexes and unsupported fuzzy modes at `tools-mcp-server/tests/integration_test.rs:320`, `tools-mcp-server/tests/integration_test.rs:354`, `tools-mcp-server/tests/integration_test.rs:395`, `tools-mcp-server/tests/integration_test.rs:496`, `tools-mcp-server/tests/integration_test.rs:539`, `tools-mcp-server/tests/integration_test.rs:588`, and `tools-mcp-server/tests/integration_test.rs:638`.
- The workspace currently depends on `sha2` in the workspace and `tools-mcp-local`, and has no `criterion` dev-dependency or `tools-mcp-local/benches` directory at `Cargo.toml:35`, `tools-mcp-local/Cargo.toml:18`, `tools-mcp-local/Cargo.toml:31`, and the observed absence of `tools-mcp-local/benches`.
- The repository has `docs/plans/`, but does not contain `docs/IFA.md`, `docs/IFA_CONFORMANCE_RULES.md`, `ifa/README.md`, `SECURITY.md`, `docs/PARALLEL_TOOL_EXECUTION.md`, or crate-local `README.md` files for the implicated crates. Available security context is `docs/tools-mcp-threat-model.md`, which frames this as a local trusted MCP server with filesystem and command surfaces as primary risk boundaries at `docs/tools-mcp-threat-model.md:1` through `docs/tools-mcp-threat-model.md:17`.

## End State

- PR1 adds measurement infrastructure first, then lands fused trigram extraction, batched deadline checks, and SHA-256 removal from memory-index freshness stamps.
- PR2 adds ignore-rule fingerprints to both the memory `IndexSnapshot` and the shared recursive scope cache so stable ignore rules allow targeted freshness for default `no_ignore=false` queries while ignore drift still forces full-scope validation.
- PR3 makes candidate verification aware of the remaining `max_results` event budget and memoizes rendered search events so the payload and text renderer share one render pass per event.
- No MCP request schema field is added or removed.
- No old compatibility path, feature flag, alternate backend selector, or public backend override is introduced.
- `TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE=1` is the only new runtime knob; it is an internal safety valve that restores the current conservative full-scope behavior for `no_ignore=false` queries.
- Search result text, structured matches, event ordering, truncation, timeout errors, fallback behavior, and additive telemetry remain compatible with the current contract.

## Behavior Changes

| Classification | Old Behavior | New Behavior | Material Concern | Ratification |
| --- | --- | --- | --- | --- |
| intentional behavior change required by the user's request | There is no Search memory benchmark target. | `tools-mcp-local` has Criterion benches for cold index build, warm query/default ignore validation, and large postings intersection. | Makes the performance series measurable. | PR descriptions include before/after bench deltas; README or bench README documents the bench workflow. |
| intentional behavior change required by a cited design constraint | Index build hashes every file into `FileStamp.hash`; changed-content fallback validates hash equality. | `FileStamp` stores metadata only; changed-content fallback re-reads and byte-compares `content == doc.content`. | Removes cold-build CPU while preserving or strengthening freshness validation. | Unit tests cover same-length rewrites and Windows-style metadata paths; SRD stale-success rules remain satisfied. |
| intentional behavior change required by the user's request | Default `no_ignore=false` warm validation always performs a full-scope scan. | If ignore-rule fingerprints are stable and the 32-query safety valve is not due, validation uses targeted result-file checks. | Reduces warm-query latency while preserving ignored-file correctness. | Unit and parity tests mutate `.gitignore`, `.ignore`, `.git/info/exclude`, and global gitignore files and assert full-scope revalidation. |
| intentional behavior change required by the user's request | There is no operational kill switch for the PR2 optimization. | `TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE=1` forces full-scope validation for `no_ignore=false` queries. | Provides a safe rollout lever without schema changes. | Unit test asserts the env var produces `freshness_full_scan_reason="ignore_rules_forced_full_scope"` or equivalent documented reason. |
| intentional behavior change required by the user's request | A candidate document scans all lines before rendering even when `max_results` will truncate early. | Candidate verification stops once the remaining event budget is proven sufficient to reproduce the rendered prefix. | Reduces Phase Two cost on dense-match files. | Unit tests compare legacy and streaming rendered output for max-results and context cases. |
| possible unintended behavior change to avoid | Deadline checks happen frequently enough that timeout tests rely on prompt failure. | Batched checks reduce `Instant::now()` calls but must keep timeout overshoot bounded. | Timeout behavior and cancellation responsiveness. | Dedicated timeout responsiveness test and unchanged cancellation checks on outer loops. |
| possible unintended behavior change to avoid | Full-scope validation currently reuses `discover_files`, which may come from `repo_scope_cache`. | Ignore-rule fingerprinting must invalidate or bypass stale shared scope snapshots before relying on targeted freshness. | Stale ignore-control files could hide file-set changes. | PR2 must update `scope_cache.rs` and add tests for ignore-file content mutation that leaves directory metadata unchanged. |
| possible unintended behavior change to avoid | `RenderedSearchEvent` is private and reconstructed by existing search-contract wrappers used by memory and ugrep paths. | New rendered-event helpers are shared internally, while existing `render_search_text` and `build_search_payload` wrappers remain for ugrep. | Response shape and fallback compatibility. | Existing search-contract tests and ugrep fallback integration tests pass unchanged. |

## Affected Files

| Path | Change |
| --- | --- |
| `tools-mcp-local/src/tools/handlers/search_memory.rs` | PR1 fused document trigram extraction, batched deadline checks, metadata-only `FileStamp`, byte-compare freshness, PR2 snapshot ignore fingerprint, PR3 streaming verification, telemetry reason wiring, tests. |
| `tools-mcp-local/src/tools/handlers/search_contract.rs` | PR3 rendered-event memoization helpers and rendered payload builder while retaining current wrappers. |
| `tools-mcp-local/src/tools/handlers/search_file_selection.rs` | PR2 returns ignore fingerprint data with memory scope discovery and preserves existing WalkBuilder semantics. |
| `tools-mcp-local/src/tools/scope_cache.rs` | PR2 adds ignore-control fingerprints to `RecursiveScopeSnapshot`, cache staleness checks, and snapshot equality. |
| `tools-mcp-local/src/tools/handlers/search_parity.rs` | PR1 and PR2 parity tests for postings equivalence and ignore-rule revalidation against forced ugrep. |
| `tools-mcp-local/src/tools/handlers/ripgrep.rs` | No behavior change intended; compile-only adjustments only if render helper signatures require wrapper adaptation. |
| `tools-mcp-server/tests/integration_test.rs` | Add only if a new server-level smoke test is needed for PR2 telemetry or kill-switch behavior; no schema-golden change is expected. |
| `tools-mcp-local/Cargo.toml` | Add `criterion` dev-dependency; remove `sha2.workspace = true` only if PR1 leaves no local SHA-256 use. |
| `Cargo.toml` | Add workspace `criterion`; remove workspace `sha2` only if no crate uses it after PR1. `tools-mcp-webfetch` currently depends on `sha2` according to `cargo metadata`, so workspace removal requires verifying that crate first. |
| `Cargo.lock` | Regenerate from dependency changes. |
| `tools-mcp-local/benches/search_memory.rs` | New Criterion benchmarks for cold index build, warm query/default ignore validation, and large postings intersection. |
| `tools-mcp-local/benches/README.md` | New benchmark workflow, baseline instructions, and machine-spec guidance. |
| `README.md` | Document new benchmarks and the PR2 internal safety valve; update dependencies only if dependency surface changes. |
| `docs/plans/PLAN_SEARCH_BACKEND_PERFORMANCE.md` | This blueprint. |

## IFA Deltas

None. This repository does not contain `docs/IFA.md`, `docs/IFA_CONFORMANCE_RULES.md`, or `ifa/README.md`, and the SRD explicitly says this tools-mcp POC is not final Hauberk design authority until Hauberk IFA, security, approval, queueing, and harness invariants are cited and reviewed at `docs/hauberk-in-memory-search-srd.md:10` through `docs/hauberk-in-memory-search-srd.md:14`.

## UI/Protocol Impact

- The MCP `Search` request schema is unchanged.
- Existing response fields and match shapes are unchanged.
- Existing additive memory telemetry remains additive.
- PR2 may change telemetry values for warm default-ignore queries: stable ignore rules should produce targeted freshness, while drift should produce full-scope freshness with a specific reason such as `ignore_file_changed`, `ignore_file_added`, `git_exclude_changed`, `global_ignore_changed`, or `ignore_rules_forced_full_scope`.
- `README.md` must document the internal `TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE=1` safety valve because it changes runtime diagnostics and performance, even though it is not an MCP schema field.

## Operation-Graph Impact

None. The change remains inside the tools-mcp `Search` request/response lifecycle; this repository has no Hauberk operation graph, queueing, approval, continuation, or resume implementation to update, and the SRD keeps those as out-of-scope POC concerns at `docs/hauberk-in-memory-search-srd.md:90` through `docs/hauberk-in-memory-search-srd.md:97`.

## Test Plan

- PR1 benchmark commit:
  - Add `criterion` as a workspace dev-dependency and `tools-mcp-local` dev-dependency.
  - Add `bench_cold_index_build`, `bench_warm_query_default_ignore`, and `bench_intersect_postings_large`.
  - Document `cargo bench -p tools-mcp-local -- --save-baseline before` and `cargo bench -p tools-mcp-local -- --baseline before`.
- PR1 fused trigram tests:
  - Add `fused_trigram_extraction_matches_legacy_postings` using a `#[cfg(test)]` legacy two-pass helper over ASCII, mixed-case ASCII, UTF-8 multibyte, edge-at-start/end, short, and empty files.
  - Assert sensitive and ASCII-folded postings maps match exactly after sorting/deduping.
  - Assert `ascii_folded_bytes_with_deadline` remains only for query literals or remove it if no callsite remains.
- PR1 deadline tests:
  - Add `batched_deadline_fires_within_worst_case_budget` with a synthetic large byte slice or fixture and a short deadline, asserting `MemoryError::timeout()` within a bounded wall-clock margin.
  - Keep cancellation checks per outer document iteration unchanged and add a regression if any cancellation call is moved.
- PR1 byte validation tests:
  - Rename `freshness_hash_validation_rejects_same_length_content_change` to byte-comparison wording and assert same-length rewrites fail through `validate_result_file_content_matches`.
  - Rename Unix and Windows hash-oriented freshness tests to metadata-fast-path and byte-validation names.
  - Drop `hash` initializers from test `FileStamp` fixtures.
- PR2 scope-cache and snapshot tests:
  - Add an ignore fingerprint type that records present control files plus absence probes for each visited directory.
  - Add `repo_scope_cache_rebuilds_when_gitignore_contents_change_without_directory_mtime_dependency`.
  - Add `mutated_gitignore_contents_trigger_full_scope`, `new_gitignore_in_subdirectory_triggers_full_scope`, `deleted_gitignore_triggers_full_scope`, `mutated_ignore_contents_trigger_full_scope`, and `no_ignore_true_skips_ignore_fingerprint`.
  - Add `.git/info/exclude` mutation tests for a fixture git repo.
  - Add global gitignore tests using `XDG_CONFIG_HOME` and the public `ignore::gitignore::gitconfig_excludes_path()` behavior rather than a shellout-only approximation.
  - Add kill-switch coverage for `TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE=1`.
  - Add or keep safety-valve coverage for the 32-query full-scope interval.
- PR2 parity tests:
  - Add `ignore_rule_change_matches_ugrep_after_revalidation`: build a memory cache with ignore rules enabled, mutate an ignore control file, search again, and assert public memory behavior matches forced ugrep behavior.
  - Include both newly ignored and newly unignored file cases if feasible.
- PR3 streaming verification tests:
  - Add `streaming_verification_stops_inside_doc_when_budget_reached` with a file containing many matches and `max_results` smaller than match count; assert verified lines drop and output remains truncated.
  - Add `streaming_verification_preserves_context_after_last_emitted_match` with `context > 0` and a small budget.
  - Add a legacy-output comparison helper for exact, seeded-regex, and fuzzy plans to prove rendered events match the current prefix for `(query, max_results, context)`.
- PR3 render memoization tests:
  - Add `render_event_memoized_payload_identical_to_legacy` comparing JSON payloads and text output from the wrapper path and rendered-event path.
  - Keep existing `render_search_text_preserves_grep_line_format`, long-line truncation, grouping, and count tests passing.
- Regression ratchets:
  - Existing `tools-mcp-server/tests/integration_test.rs` Search backend tests must continue passing.
  - Existing `search_parity.rs` public-vs-forced-ugrep tests must continue passing where `ugrep` is installed.
  - Add no test that asserts absolute performance timings as correctness, except bounded timeout responsiveness with a generous multiplier.

## Out-of-Scope

- Do not flip the `fixed_strings` default or change the schema description contradiction; track that as a separate contract proposal.
- Do not implement cross-glob index reuse in this series.
- Do not implement ugrep path-list cache reuse in this series.
- Do not add an on-disk persistent index, file-system watchers, semantic/ranked search, embeddings, or Read acceleration.
- Do not alter WebFetch, Git, file mutation tools, path policy, or MCP router behavior.
- Do not introduce public backend selection flags, compatibility aliases, or dual old/new response modes.

## Verification

- Planning verification already performed:
  - Read the supplied plan at `C:\Users\Daniel\.claude\plans\plan-it-detailed-and-whimsical-grove.md`.
  - Read `AGENTS.md`.
  - Read available repository docs: `README.md`, `docs/hauberk-in-memory-search-srd.md`, and `docs/tools-mcp-threat-model.md`.
  - Confirmed absent required Hauberk-specific docs: `docs/IFA.md`, `docs/IFA_CONFORMANCE_RULES.md`, `ifa/README.md`, `SECURITY.md`, and `docs/PARALLEL_TOOL_EXECUTION.md`.
  - Inspected relevant source paths and tests in `search_memory.rs`, `search_contract.rs`, `search_file_selection.rs`, `scope_cache.rs`, `ripgrep.rs`, `search_parity.rs`, and `tools-mcp-server/tests/integration_test.rs`.
  - Consulted local installed `ignore` crate source for `WalkBuilder` ignore-file behavior and global gitignore path resolution.
- Implementation verification for each PR:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test -p tools-mcp-local`
  - `cargo test -p tools-mcp-server`
  - `cargo test --workspace`
  - `cargo bench -p tools-mcp-local -- --save-baseline before` before PR1 behavior changes, then `cargo bench -p tools-mcp-local -- --baseline before` after each PR.
  - Manual MCP smoke test with `MCP_SKIP_HEADERS=true RUST_LOG=error cargo run -p tools-mcp-server --release` and a `Search` call that observes `backend: "memory"`, `index_cache`, `freshness_scope`, and PR2 `freshness_full_scan_reason`.
  - PR2 manual smoke: run a default `no_ignore=false` search twice, mutate `.gitignore`, run the search again, and verify the second response reflects the changed ignore rules with an ignore-specific full-scan reason.
- Repo-specific verification note:
  - The blueprint skill recommends `just verify` and `just cov`, but this repository's observed `justfile` has no `verify` or `cov` recipe. Use the explicit Cargo commands above unless those recipes are added.

## Risks

- The largest correctness risk is PR2 interacting with `repo_scope_cache`; ignore fingerprints must invalidate shared scope snapshots as well as memory index snapshots.
- Global gitignore parity is subtle. Use the `ignore` crate's public global path helper and tests under controlled `HOME`/`XDG_CONFIG_HOME` rather than approximating with `git config` alone.
- Batched deadlines can create timeout overshoot if intervals are too large. Keep intervals local constants with tests and comments that state the maximum responsiveness budget.
- Removing SHA-256 from `FileStamp` is safe only if changed-metadata validation byte-compares the re-read file against `doc.content` before success. Do not replace hash validation with metadata-only validation on Windows or platforms where the change marker is not authoritative.
- Streaming verification must prove rendered prefixes are identical for context-heavy truncation cases. Treat any difference in match/context labeling, event order, or `count` as a correctness failure.
- Render memoization touches shared response construction used by both memory and ugrep paths. Keep compatibility wrappers and existing response-shape tests until every caller is migrated deliberately.
- Benchmarks can become noisy on developer machines. Use them to compare relative deltas on the same machine and treat wins below 10 percent on the targeted bench as a pause-and-reassess signal.
