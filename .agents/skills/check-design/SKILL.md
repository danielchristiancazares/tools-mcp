---
name: check-design
description: "RUST_DESIGN and repo-rule conformance audit for a file, module, crate, or diff. Use when the user wants a cited review for forbidden patterns, weakened type boundaries, ownership violations, risky refactors, or architectural drift before or after code changes."
---

# Check Design

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Perform a read-only RUST_DESIGN and repo-rule conformance audit.

Central test: does the target introduce or render reachable an invalid state, ambiguous authority boundary, weakened proof, forbidden pattern, or repo-rule violation reachable where design rules require it to be impossible, explicit, or boundary-local?

Material design concerns:

- RUST_DESIGN conformance
- impossible-state modeling
- proof-carrying state
- authority ownership
- typestate/ownership boundaries
- lifecycle/equality/clone/move semantics when correctness-relevant
- policy/lifecycle representation
- persistence/queueing/approval/rendering/state-machine semantics
- behavior impact for diffs
- explicit repo guardrails

Treat suspect patterns as design bugs until code evidence proves the boundary or ownership model is safe.

If the user only needs to assess behavioral regression risk, prefer `$check-regressions`.

## Inputs

Accept file paths, modules, crates, staged diffs, unstaged diffs, or pasted/referenced patches. If no scope is provided, ask for one.

## Required Evidence

Before findings, read:

- `docs/IFA.md` in full
- code, tests, docs, and `ifa/` artifacts defining the scope
- `scripts/ci/impossible_state_escape_hatches_allowlist.txt` when the scope may rely on an allowlisted escape hatch
- `docs/PARALLEL_TOOL_EXECUTION.md` and `SECURITY.md` when tool execution, approvals, queueing, recovery, sandboxing, or security-sensitive behavior is involved

Cite `file:line` for each load-bearing claim.

## Scope Discipline

Under-flagging material design violations is worse than over-flagging them. Over-reporting immaterial style is scope drift.

A finding is valid only when it affects a material design concern or violates an explicit repo guardrail.

Do not report private naming, decomposition, or stylistic preference unless a project rule makes it relevant.

## Checks

Check repo-defined surfaces and core logic for:

- `Option<T>` or pseudo-optional variants
- bare `bool` for policy, authority, or lifecycle state
- wildcard policies such as `Any`, `All`, `Unrestricted`, or `AllowAll`
- stored `Result`, `Result<T, ()>`, or `Result<T, NotFound>` used as optionality
- external ambiguity escaping the boundary layer
- generic lifecycle/container enum names that hide consequences
- `Copy` or casual `Clone` on typestate, authority, proof, or capability carriers
- populated error variants used to represent states that should be unrepresentable, instead of using `!` or `Infallible`
- `Arc<Mutex<_>>`, `Arc<RwLock<_>>`, or `Rc<RefCell<_>>` carrying domain truth
- `.unwrap()`, `.expect()`, `unreachable!()`, or fallback arms carrying proof

Check explicit repo guardrails: `NonEmptyString` when non-empty text is required, hoisted `use` imports, collapsible `if`, no wildcard imports, and repo-relative paths where required.

For each issue that remains after analysis, prefer narrowed types, typestate splits, ownership rewrites, consequence-first enums, or boundary collapse over suppressions and fallback branches.

For diffs or patches, classify behavior impact as exactly one:

- `Preserved`: no observable change in behavior, traced to an observer.
- `Intentional change`: behavior change required by the patch, with old and new behavior named.
- `Unclear`: trace incomplete; behavior could change but observer evidence is missing.

Do not declare preservation without tracing an observer: caller, test, doc claim, snapshot, or downstream consumer.

## Behavior Change Authority

A design improvement does not silently authorize changed behavior. For behavior-changing findings, state old behavior, new behavior, observer, explicit justification, and whether author approval is required.

## Severity

Use `Blocker`, `Serious`, `Minor`, or `Nit`.

`Blocker` means a clear design violation or behavior-impact ambiguity that blocks proceeding. `Serious` means material design risk. `Minor` is bounded. `Nit` is explicit repo hygiene with no material correctness impact and MUST NOT drive broad refactors.

## Output

Produce headings in this order: `Findings`, `Swept Clean`, `Open Questions`, `Behavior Impact`, `Verdict`.

For each finding include severity, `file:line`, violated rule or guardrail, why it matters, and minimal upstream fix.

`Swept Clean` lists material concerns checked and disproved. `Open Questions` lists only unresolved intent or missing context; say `None.` when empty. `Behavior Impact` is required for diffs and patches. `Verdict` states whether the scope is design-clean enough to proceed.

## Hard Rules

You MUST cite every load-bearing claim, verify patterns against actual boundaries and ownership, distinguish explicit guardrails from preference, prefer compile-time impossibility, and separate behavior changes from design cleanup.

You MUST NOT accept a pattern as correct merely because tests pass, suggest suppressions only when the rule truly does not apply, declare behavior preserved without tracing an observer, edit code unless asked, or inflate findings with immaterial style.
