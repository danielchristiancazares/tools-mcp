# Neuro-Symbolic Code Analysis System
## Requirements Document v1.0

---

## 1. EXECUTIVE SUMMARY

**Purpose:** Build a system that combines LLM code analysis with formal verification to eliminate false positives and prove correctness of security/correctness claims.

**Target User:** Solo developer analyzing codebases for bugs, security vulnerabilities, and correctness.

**Core Value Proposition:**
- LLMs find potential issues (high recall, low precision)
- Symbolic verification eliminates false positives (provable correctness)
- Result: High-confidence bug reports without noise

**Success Criteria:**
- 90%+ reduction in false positives vs LLM-only analysis
- <5 minute analysis time for typical file (500 LOC)
- Zero false negatives (if Z3 says safe, it's provably safe)

---

## 2. FUNCTIONAL REQUIREMENTS

### 2.1 Core Capabilities

**FR-1: Code Ingestion**
- **MUST** accept Rust source files (.rs)
- **SHOULD** support workspaces/crates with multiple modules
- **MAY** support additional languages via plugin architecture
- **MUST** handle UTF-8 encoding
- **MUST** gracefully reject binary files
- **MUST** support stdin for pipeline integration

**FR-2: LLM Analysis**
- **MUST** integrate with Anthropic Claude API
- **MUST** support configurable prompts for Rust-specific analysis types:
  - Division by zero / integer overflow
  - Panic safety (unwrap/expect/indexing)
  - Unsafe block invariants
  - Logic errors / unreachable code
  - Dangerous `unsafe` FFI usage
- **MUST** parse LLM output into structured findings
- **MUST** handle LLM API failures gracefully (retry with backoff)
- **MUST** support prompt caching for cost optimization

**FR-3: Constraint Extraction**
- **MUST** parse Rust AST (functions, impl blocks, modules) to extract:
  - Variable bindings and types (from MIR-like data)
  - Conditional branches (if/else/match)
  - Loop bounds (`for`, `while`, iterators)
  - Function calls with generics/traits resolved when possible
  - Return values and early exits (`?`, `return`, `panic!`)
- **MUST** identify data/control flow paths from:
  - External inputs (CLI args, env vars, network/file reads)
  - FFI boundaries / unsafe blocks
  - To sensitive operations (filesystem, networking, `unsafe` memory)
- **MUST** handle nested conditions (depth ≥5)
- **MUST** track variable state across basic blocks

**FR-4: Symbolic Verification**
- **MUST** integrate Z3 SMT solver
- **MUST** convert code constraints to Z3 format
- **MUST** verify LLM claims:
  - Path feasibility: "Can this code path execute?"
  - Safety properties: "Is overflow possible here?"
  - Reachability: "Can malicious input reach this operation?"
- **MUST** generate counterexamples when claim is false
- **MUST** timeout on unsolvable constraints (30s default, configurable)

**FR-5: Results Reporting**
- **MUST** output structured JSON with:
  - Finding ID
  - Severity (critical/high/medium/low)
  - Location (file, line, column)
  - LLM hypothesis
  - Verification status (verified/refuted/unknown)
  - Proof/counterexample
  - Confidence score
- **SHOULD** support human-readable text output
- **SHOULD** support SARIF format for IDE integration
- **MUST** include actionable remediation advice

### 2.2 Analysis Types (Priority Order)

**AT-1: Integer Safety & Division by Zero (P0)**
- Detect all integer arithmetic/division operations
- Verify denominators and modulus operands are non-zero
- Ensure arithmetic cannot overflow/underflow given type ranges

**AT-2: Panic Path Analysis (P0)**
- Track `unwrap()`, `expect()`, indexing, `panic!` macros
- Verify guard conditions ensure these paths are safe
- Flag `Result`/`Option` misuse and missing error handling

**AT-3: Unsafe Block Verification (P1)**
- Identify invariants claimed around `unsafe` blocks/FFI calls
- Verify pointer aliasing, lifetime, and bounds assumptions
- Ensure `unsafe` sections encapsulate minimal code

**AT-4: Logic & Reachability Errors (P1)**
- Detect contradictory conditions, dead code, unreachable branches
- Highlight tautologies or impossible match arms
- Identify inconsistent `match` exhaustiveness handling

**AT-5: Path/Resource Misuse (P2)**
- Identify user-controlled paths reaching filesystem/network APIs
- Verify sanitization and canonicalization when relevant
- Surface potential privilege escalations in CLI/daemon contexts


---

## 3. NON-FUNCTIONAL REQUIREMENTS

### 3.1 Performance

**NFR-P1: Latency**
- Single file analysis: <5 minutes (500 LOC)
- LLM call: <30 seconds
- Z3 verification per claim: <30 seconds (timeout)
- Total pipeline: <10 minutes for 2000 LOC file

**NFR-P2: Throughput**
- Support batch processing of multiple files
- Parallelization: N concurrent files (N = CPU cores)
- Rate limiting: respect API quotas (50 req/min for Claude)

**NFR-P3: Resource Usage**
- Memory: <2GB per analysis task
- Disk: <100MB cache per project
- CPU: Gracefully handle OOM, timeout

### 3.2 Reliability

**NFR-R1: Fault Tolerance**
- LLM API failure: Retry 3x with exponential backoff
- Z3 timeout: Report as "unknown" rather than crash
- Syntax error in code: Report parse failure, continue with other files
- Network interruption: Cache partial results, resume

**NFR-R2: Correctness Guarantees**
- **CRITICAL**: Zero false negatives on verified safe paths
  - If Z3 says "safe", must be provably safe
  - If uncertain, report as "unknown", never "safe"
- Soundness > Completeness
- Document limitations explicitly

**NFR-R3: Availability**
- Work offline after initial setup
- Cache Z3 solver binaries
- Graceful degradation without API access (skip LLM, use heuristics)

### 3.3 Security

**NFR-S1: Data Privacy**
- **MUST NOT** send code to LLM if `--offline` flag set
- **MUST** support local LLM deployment (Ollama integration)
- **MUST** sanitize filepaths before logging
- **SHOULD** offer option to redact sensitive values

**NFR-S2: Credential Management**
- Support environment variables for API keys
- Support keyring integration
- **MUST NOT** log API keys
- **MUST NOT** commit API keys to config files

**NFR-S3: Code Execution Safety**
- **MUST NOT** execute user code
- **MUST** sandbox any dynamic analysis
- Z3 queries only (no `eval()`, no `exec()`)

### 3.4 Usability (Solo Dev Context)

**NFR-U1: Setup Time**
- Installation: <5 minutes
- Configuration: <2 minutes
- First analysis: <1 minute

**NFR-U2: Learning Curve**
- Basic usage: <10 minutes to first result
- Advanced features: <1 hour to master
- Documentation: Examples for every feature

**NFR-U3: Integration**
- CLI tool with standard conventions
- Exit codes: 0 (clean), 1 (issues found), 2 (error)
- Pre-commit hook support
- CI/CD integration examples

**NFR-U4: Debuggability**
- Verbose mode showing: LLM prompts, Z3 queries, intermediate results
- Log levels: ERROR, WARN, INFO, DEBUG, TRACE
- Ability to reproduce any run (deterministic given same inputs)

---

## 4. EDGE CASES & FAILURE MODES

### 4.1 Input Edge Cases

**EC-1: Malformed Code**
- Syntax errors → Parse failure, report, continue
- Incomplete code → Warn, attempt best-effort analysis
- Non-UTF8 encoding → Attempt decode with fallback encodings
- Giant files (>10K LOC) → Chunk analysis or skip with warning

**EC-2: Complex Control Flow**
- Deeply nested loops (>10 levels) → Flatten or approximate
- Recursive functions → Track to depth limit (default 5)
- Goto statements → Build CFG carefully
- Exception handling → Model exception paths

**EC-3: Dynamic Behavior**
- `eval()` / `exec()` → Flag as unsound, cannot verify
- Reflection / `getattr()` → Conservative approximation
- Dynamic imports → Assume worst case
- Metaprogramming → Warn, skip verification

**EC-4: External Dependencies**
- Missing imports → Stub with unknown values
- Third-party library calls → Trust annotations or skip
- Database schemas → Require user-provided schema or skip
- Network calls → Model as arbitrary return values

### 4.2 LLM Failure Modes

**EC-5: LLM Errors**
- Rate limiting → Backoff, retry, queue
- Timeout → Retry once, then fail gracefully
- Malformed JSON → Retry with stricter prompt, then parse best-effort
- Hallucination → This is why we verify! Z3 catches it
- Context window exceeded → Chunk code, analyze parts

**EC-6: LLM Misunderstanding**
- Misidentifies issue location → Re-prompt with context
- Invents non-existent code → Z3 verification fails, caught
- Misses issue → Acceptable (verification phase won't catch what LLM misses, but that's by design)
- Incorrect severity → Override with heuristics based on issue type

### 4.3 Z3 Failure Modes

**EC-7: Solver Issues**
- Timeout (>30s) → Report "unknown", don't block
- Unsatisfiable constraints (unsat) → Code path impossible, report
- Unknown result → Conservative: report as potential issue
- Out of memory → Simplify constraints, retry once

**EC-8: Constraint Complexity**
- Non-linear arithmetic → Switch to approximation
- String constraints → Use Z3 string theory (limited)
- Floating point → Use Z3 float theory (imprecise)
- Arrays → Use Z3 array theory (scalability issues)

### 4.4 Integration Edge Cases

**EC-9: Environment**
- Missing Z3 binary → Download/build on first run, cache
- Incompatible Z3 version → Pin version, verify
- Rust toolchain mismatch → Require Rust ≥1.80 (stable) and `cargo`
- Missing dependencies → Clear error messages with install instructions

**EC-10: Concurrent Usage**
- Multiple runs in same directory → Separate cache per PID
- Shared cache corruption → Lock-free design or file locking
- Partial results from crashed run → Cleanup on startup

---

## 5. ARCHITECTURE DECISIONS

### 5.1 Build vs Buy

| Component | Decision | Rationale |
|-----------|----------|-----------|
| **LLM** | Buy (API) | Cost-effective for solo dev, no training needed |
| **SMT Solver** | Buy (Z3) | Mature, proven, free |
| **AST Parser** | syn crate | Battle-tested Rust parser |
| **CFG Builder** | Build (minimal) | Simple, project-specific |
| **Constraint Extractor** | Build | Core IP, custom logic needed |
| **Orchestration** | Build (simple) | Just a pipeline, don't overengineer |

### 5.2 Tech Stack

**Core:**
- Language: Rust (edition 2021, Rust ≥1.80)
- LLM: Anthropic Claude (Sonnet 4.5)
- SMT: Z3 via `z3` crate (static linking)
- AST: `syn` + `quote` crates

**Optional:**
- Multi-language: `tree-sitter` (future languages)
- Local LLM: Ollama + Qwen2.5-Coder
- Graph analysis: petgraph / rustc-hir (future)

**Infrastructure:**
- CLI: `clap` + `color-eyre`
- Config: TOML via `toml_edit`
- Logging: `tracing` + `tracing-subscriber`
- Testing: `cargo test`, `proptest` for property tests

### 5.3 Data Flow

```
Input Code
    ↓
┌──────────────────────────┐
│  1. Parse & Validate     │
│  - AST parsing           │
│  - Syntax checking       │
└────────┬─────────────────┘
         ↓
┌──────────────────────────┐
│  2. LLM Analysis         │
│  - Send code to Claude   │
│  - Get hypotheses        │
└────────┬─────────────────┘
         ↓
┌──────────────────────────┐
│  3. Constraint Extract   │
│  - Build CFG             │
│  - Extract conditions    │
│  - Track data flow       │
└────────┬─────────────────┘
         ↓
┌──────────────────────────┐
│  4. Verification         │
│  - Convert to Z3         │
│  - Check satisfiability  │
│  - Generate proofs       │
└────────┬─────────────────┘
         ↓
┌──────────────────────────┐
│  5. Report Generation    │
│  - Merge LLM + Z3        │
│  - Format output         │
│  - Write results         │
└──────────────────────────┘
```

### 5.4 What NOT to Build (YAGNI)

❌ **Don't Build:**
- Custom LLM fine-tuning (use API)
- Custom SMT solver (use Z3)
- Full IDE integration (CLI first)
- Web UI (CLI sufficient for solo dev)
- Database for results (files are fine)
- Distributed processing (single machine OK)
- Real-time monitoring (batch is fine)
- User authentication (single user)
- Plugin marketplace (hardcode initially)

✅ **Do Build:**
- Core pipeline (parse → LLM → verify → report)
- Constraint extraction (this is the hard part)
- Good error messages (save debugging time)
- Comprehensive tests (prevent regressions)
- Clear documentation (future you will forget)

---

## 6. TESTING STRATEGY

### 6.1 Unit Tests

**Coverage Target: 80%+ for core logic**

**UT-1: Parser Tests**
- Valid Rust modules → Successful parse (syn)
- Syntax errors → Graceful failure with span info
- Edge cases: empty file, doc-comments, unicode identifiers

**UT-2: Constraint Extraction Tests**
- Simple if: `if x > 0` → Extract `x > 0`
- Compound: `if x > 0 and y < 10` → Both constraints
- Negation: `if not x` → Correct boolean logic
- Loops: while/for → Proper encoding

**UT-3: Z3 Integration Tests**
- Satisfiable constraints → Returns `sat` + model
- Unsatisfiable → Returns `unsat`
- Timeout → Handles gracefully
- Invalid constraints → Clear error

**UT-4: LLM Integration Tests**
- Mock responses → Parse correctly
- Malformed JSON → Retry logic
- API errors → Backoff and retry
- Cost tracking → Correct token counts

### 6.2 Integration Tests

**IT-1: End-to-End Pipelines**
- Known vulnerable code → Detects + verifies
- Safe code → Reports clean
- Mixed codebase → Correct results per file

**IT-2: Real-World Samples**
- Common vulnerabilities (OWASP Top 10)
- Open source projects with known bugs
- Synthetic test cases from literature

### 6.3 Property-Based Tests

**PT-1: Invariants**
- If Z3 says unsat, no model should exist
- If Z3 says sat, model should satisfy constraints
- Verified findings must have proof
- Refuted findings must have counterexample

**PT-2: Fuzzing**
- Random valid Rust snippets → No crashes
- Random invalid inputs → Graceful errors
- Property: Never claim "safe" if uncertain

### 6.4 Regression Tests

**RT-1: Golden Outputs**
- Freeze known-good results for representative Rust crates
- Run on every commit
- Flag any changes for review

**RT-2: Performance Regression**
- Track analysis time over versions
- Alert if >20% slowdown

---

## 7. DEPLOYMENT & OPERATIONS

### 7.1 Installation

**Delivery Method:** Cargo crate / prebuilt binary

```bash
cargo install neurosym

# First run auto-builds Z3 (if static lib missing)
neurosym --setup

# Ready to use
neurosym analyze src/main.rs
```

**Dependencies:**
- Rust toolchain ≥1.80 (stable) with `cargo`
- libz3 (or build from source)
- OpenAI/Anthropic API access
- Optional: rust-analyzer for richer metadata

### 7.2 Configuration

**Config File:** `.neurosym.toml` (project root)

```toml
[llm]
provider = "anthropic"  # or "ollama" for local
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
max_tokens = 16000

[verification]
solver = "z3"
timeout_seconds = 30
enable_cache = true

[analysis]
check_types = ["division-by-zero", "panic", "unsafe"]
severity_threshold = "medium"  # low|medium|high|critical

[output]
format = "json"  # json|sarif|text
verbose = false
include_proofs = true
```

### 7.3 Monitoring (Solo Dev Context)

**Keep It Simple:**
- Log to stdout/file
- Track: runtime, cost (API tokens), findings count
- Monthly reports via cron + email

**Don't Build:**
- Prometheus metrics (overkill)
- Grafana dashboards (unnecessary)
- Alerting system (just check logs)

---

## 8. MAINTENANCE & EVOLUTION

### 8.1 Version Strategy

**v0.1: Prototype (1-2 weeks)**
- Single Rust source file (≤500 LOC)
- Detect division/modulo by zero with constant guards
- Prompt Claude for context, verify with linear Z3 constraints
- JSON-only output

**v1.0: MVP (2-4 weeks)**
- Single-crate scope, no macros/generics beyond derives
- Panic path analysis (`unwrap`, `expect`, indexing)
- Basic CFG + value tracking
- CLI reports verified/unknown findings

**v1.1: Stabilization (1 week)**
- Bug fixes from dogfooding
- Performance optimization
- Better error messages + logging

**v2.0: Feature Expansion (4-6 weeks)**
- Unsafe block verification (limited patterns)
- Workspace/multi-module support
- SARIF output, CI integration

**v3.0: Advanced (future)**
- Trait/generic-aware dataflow, MIR integration
- Local LLM support (Ollama)
- IDE plugin / live diagnostics

### 8.2 Documentation Requirements

**User Docs:**
- README with quick start
- Installation guide
- Usage examples for each analysis type
- Troubleshooting guide
- FAQ

**Developer Docs:**
- Architecture overview
- Code walkthrough
- How to add new analysis types
- Testing guidelines
- Contribution guide

### 8.3 Success Metrics

**Leading Indicators (Track Weekly):**
- Analysis success rate (% runs without errors)
- False positive rate (verified / LLM claims)
- Average runtime per file
- API cost per analysis

**Lagging Indicators (Track Monthly):**
- Bugs found in real codebases
- Time saved vs manual review
- User satisfaction (if public)

---

## 9. RISKS & MITIGATIONS

| Risk | Impact | Probability | Mitigation |
|------|--------|-------------|------------|
| LLM API cost too high | High | Medium | Cache aggressively, use prompt compression |
| Z3 timeout on real code | Medium | High | Simplify constraints, chunk analysis |
| False negatives in verification | Critical | Low | Conservative defaults, extensive testing |
| Scope creep | Medium | High | Strict MVP definition, say no to features |
| Burnout (solo dev) | High | Medium | Timebox work, ship imperfect v1 |
| Poor code quality (rapid dev) | Medium | High | Mandatory tests, code review (even self) |

---

## 10. MVP DEFINITION (Ruthless Prioritization)

**Must Have (Ship-Blockers):**
1. Parse single Rust file (syn) and locate division/panic sites
2. Claude API integration for one hypothesis type (division by zero)
3. Encode simple linear constraints into Z3 to refute/confirm
4. CLI + JSON output for verified/unknown findings
5. Basic error handling (API failure, timeout, parse failure)
6. README with install/run instructions

**Should Have (v1.1):**
7. Panic-path detection (unwrap/expect/index)
8. Retry logic + caching for LLM
9. Verbose logging/`--trace` mode
10. Config file support

**Could Have (v2.0):**
11. Workspace/multi-file analysis
12. SARIF output + CI integration
13. Pre-commit hook
14. Performance tuning / parallelism

**Won't Have (Out of Scope):**
15. GUI/web interface
16. Multi-language beyond JS
17. Custom LLM training
18. Real-time analysis

---

## 11. ACCEPTANCE CRITERIA

**The system is ready to ship when:**

✅ **Functionality:**
- Analyzes a ≤500 LOC Rust file end-to-end
- Detects/verifies curated division-by-zero or panic bugs
- Produces JSON output with verification status + proof/counterexample
- Gracefully handles parse, API, or solver failures

✅ **Quality:**
- 80%+ test coverage on core logic
- Zero critical bugs in test suite
- Documentation lets a new user run first analysis in <10 minutes

✅ **Performance:**
- Single file analysis completes in <3 minutes
- API costs <$0.05 per typical file

✅ **Usability:**
- Installation works via `cargo install neurosym`
- CLI has `--help` plus sample commands
- Error messages guide next steps

**Dogfooding Test:**
Run on 10 curated Rust files/crates (mix of buggy/safe). At least 7/10 must:
- Complete without crashing
- Provide useful findings (verified bug or documented “unknown”)
- Finish within performance budget

---

## 12. OUT OF SCOPE (Explicit Non-Goals)

This system will **NOT**:
- ❌ Replace human security audits (augments, doesn't replace)
- ❌ Handle all programming languages (focus on Rust for MVP)
- ❌ Prove full program correctness (target specific bug classes)
- ❌ Work without internet (Claude API required for MVP)
- ❌ Scale to enterprise (single developer focus)
- ❌ Provide legal guarantees (best-effort tool)
- ❌ Compete with commercial tools (personal productivity tool)

---

## APPENDIX A: Example Workflow

```bash
# Install
$ cargo install neurosym

# Setup (one-time)
$ export ANTHROPIC_API_KEY="sk-..."
$ neurosym --setup

# Analyze single file
$ neurosym analyze src/main.rs

# Output:
{
  "file": "src/main.rs",
  "findings": [
    {
      "id": "DIV-001",
      "type": "division-by-zero",
      "location": {"line": 42, "column": 15},
      "severity": "high",
      "llm_hypothesis": "Variable 'count' could be zero at division",
      "verification": "VERIFIED",
      "proof": "Z3 found satisfying assignment: count=0, items=[]",
      "confidence": 0.95,
      "recommendation": "Add guard `if count > 0` before division"
    }
  ],
  "summary": {
    "total_findings": 1,
    "verified": 1,
    "refuted": 0,
    "unknown": 0,
    "runtime_seconds": 23.4,
    "cost_usd": 0.08
  }
}

# Batch analysis
$ neurosym analyze crates/*/src/**/*.rs --output=results.json

# CI integration
$ neurosym analyze . --format=sarif --fail-on=high
# Exit code 1 if high-severity issues found
```

---

## APPENDIX B: Cost Model

**Claude API (Sonnet 4.5):**
- Input: $3 per 1M tokens
- Output: $15 per 1M tokens

**Typical File (500 LOC):**
- Input tokens: ~2,000 (code + prompt)
- Output tokens: ~1,000 (findings)
- Cost: (2K × $3 + 1K × $15) / 1M = $0.021 per file

**Monthly Usage (Solo Dev):**
- 100 files analyzed
- Total cost: $2.10/month

**With Prompt Caching:**
- 90% cache hit rate on repeated analyses
- Cost: ~$0.50/month

**Conclusion:** Cost is negligible for solo dev.

---

**END OF REQUIREMENTS DOCUMENT**
