---
name: doublecheck
description: "Use when you want a hostile second pass on something already reviewed. Run this after check-plan, check-design, or check-regressions to challenge assumptions and catch what was missed."
---

# Doublecheck

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Perform a hostile second-pass review.

Central test: can the artifact's load-bearing promises survive adversarial tracing against the real codebase, spec, diff, tests, docs, IFA artifacts, and runtime-relevant behavior?

Treat the artifact as wrong until evidence proves otherwise.

Material promises include behavior preservation, implementation readiness, design invariants, security/authority, persistence, queueing, approval, rendering, state machines, commit-message accuracy, test validity, and prior-review claims.

Use `$check-plan` for first-pass plan readiness, `$check-regressions` for regression-only review, `$check-design` for pure RUST_DESIGN conformance, and `$security-audit` for exploit-oriented security findings.

## Inputs

Review attached files, pasted text, referenced paths, diffs, staged changes, and trailing prompt text.

Accept plans, design docs, diffs, staged changes, PRs, code, decisions, tests, commit messages compared to staged diffs, and prior reviews.

Read referenced files first. If multiple artifacts are referenced, review all and report conflicts. If the target is ambiguous or missing, ask for the artifact or scope.

## Required Evidence

Before findings, read:

- the artifact in full
- `docs/IFA.md` in full
- codepaths, tests, docs, and `ifa/` artifacts touched by the artifact or used to support its claims
- relevant material in `references/review-by-artifact.md` when that file exists
- `docs/PARALLEL_TOOL_EXECUTION.md` and `SECURITY.md` when tool execution, approvals, queueing, recovery, sandboxing, or security-sensitive behavior is involved

Cite `file:line` for each load-bearing claim.

## Scope Discipline

Under-flagging material failures is worse than over-flagging them. Over-reporting immaterial issues is scope drift.

A finding is valid only when it falsifies, weakens, or leaves unresolved a material promise.

Do not report private style, naming, or decomposition concerns unless they affect correctness, behavior, authority, state, persistence, downstream observations, RUST_DESIGN conformance, or claim truth.

## Behavior Change Authority

A hidden behavior change is a valid finding even when the new behavior is cleaner.

For every behavior-changing finding, state old behavior, new behavior, observer, the artifact's justification for the change, and whether explicit author approval is required.

## Workflow

1. Restate claimed intent in one sentence.
2. Enumerate hard promises: what will happen, what will not change, preserved invariants, and still-valid tests/docs/IFA artifacts.
3. Apply artifact-specific checks from `references/review-by-artifact.md` when that file covers the current artifact type.
4. Trace load-bearing paths through entry points, boundaries, branching, ownership, state transitions, side effects, persistence, errors, callers, and consumers.
5. Build the adversary list without filtering, including:
   - regressions
   - user-visible or script-visible regressions hidden inside "safe refactor" claims
   - hidden side effects
   - edge cases
   - contradictions
   - house-rule violations
   - invalid states made reachable
   - omitted failure modes
   - tests that do not prove what they claim
6. Disprove each concern by reading current evidence.
7. Promote only surviving or unresolved material concerns to findings.
8. Put high-value disproved concerns in `Swept Clean`.

Hauberk checks: proof loss; typestate weakening; repo-surface `Option<T>`, bare `bool`, or wildcard policies; boundary ambiguity in core logic; proof carried by `.unwrap()`, `.expect()`, `unreachable!()`, or fallback arms; lock/clone ownership leaks carrying domain truth; hidden behavior changes; missing docs/IFA updates; unsupported commit-message claims; tests that do not prove their claims.

## Severity

Use `Blocker`, `Serious`, `Minor`, or `Nit`.

`Blocker` means do not proceed. `Serious` means material revision required. `Minor` is bounded and non-blocking. `Nit` has no material readiness impact and MUST NOT drive required fixes.

## Output

Produce headings in this order: `Findings`, `Swept Clean`, `Open Questions`, `Verdict`.

For each finding include severity, category, title, failed promise or unsupported claim, observable regression surface when applicable, exact `file:line` evidence, concrete impact, and minimal upstream fix.

For each `Blocker` or `Serious` finding include a concrete test or proof-of-failure that would demonstrate the issue, plus a trace showing it is not merely a behavioral regression.

If no findings survive, say `No findings.`

`Swept Clean` lists strongest concerns disproved with evidence. `Open Questions` lists only unresolved intent, missing artifacts, or missing runtime evidence; say `None.` when empty.

Verdict MUST be exactly `Reject`, `Needs revision`, or `Adversarial pass complete`, justified in 2-5 sentences.

## Hard Rules

You MUST be hostile to claims, cite `file:line`, trace current code instead of trusting prior work, prefer root-cause fixes, distinguish material findings from optional improvements, and show evidence for strong concerns you disproved.

You MUST NOT speculate, validate claims by default, accept “no behavior change” without tracing an observer, edit code unless asked, report immaterial style as adversarial findings, or compare severity labels across skills.
