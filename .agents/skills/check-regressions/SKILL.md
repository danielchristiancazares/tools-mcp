---
name: check-regressions
description: "Use when you want to know what could break for users, scripts, or downstream consumers. Run this after check-design or blueprint to catch behavioral regressions before they ship."
---

# Check Regressions

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Perform a baseline-vs-proposed behavioral regression review.

Central test: for each plausible material regression claim, can a dedicated adversarial disprover kill the claim with concrete `file:line` evidence?

A claim becomes a finding only when it survives adversarial refutation, needs runtime evidence, or remains unresolved after required disprover attempts.

Material observation surfaces: CLI flags/commands/output/exits; TUI/rendering/prompts/warnings/help/auth-visible output; config/defaults; persisted-state reload meaning; public API and scripts; harness behavior; snapshots/tests/downstream consumers; ordering/retries/resume/queueing/dispatch/cleanup/continuation; approval/security UX/redaction/sandbox behavior.

Use this skill to assess behavioral regression risk, not first-pass plan readiness, broad hostile review, pure design conformance, or exploit-oriented security audit.

## Inputs

Accept plans, design artifacts, staged diffs, unstaged diffs, named files/modules/crates, pasted patches, or existing code believed to preserve behavior.

If the scope is ambiguous, ask for the missing artifact or target. Do not guess.

## Required Evidence

Before producing claims, read:

- `AGENTS.md`
- `docs/IFA.md`
- the artifact under review in full
- codepaths, tests, docs, snapshots, and downstream consumers that define baseline behavior
- relevant `ifa/*.toml` artifacts when invariant meaning, authority ownership, persistence, classification, or proof boundaries may change
- `docs/PARALLEL_TOOL_EXECUTION.md` and `SECURITY.md` when tool execution, approvals, queueing, recovery, security-sensitive behavior, or sandbox boundaries are involved

## Scope Discipline

Under-enumerating material regression risk is worse than over-enumerating it. Over-reporting immaterial concerns is scope drift.

A regression claim is valid only when it is concrete, observable, falsifiable, narrow, and tied to a material observation surface.

Architecture matters only when it changes or may change observable behavior.

## Workflow

1. State scope in one sentence.
2. Identify material observation surfaces.
3. For each relevant surface, produce at least one plausible claim or state why none exists.
4. Build baseline behavior from current code, tests, docs, snapshots, and consumers.
5. Build proposed behavior from the plan, diff, or implementation.
6. Enumerate candidate claims from baseline/proposed deltas.
7. Freeze the claim set.
8. Assign exactly one dedicated disprover to each claim.
9. Require each disprover to trace end to end and attempt refutation.
10. Re-check each result against live code.
11. Carry forward only surviving, narrowed, runtime-dependent, or unresolved claims.
12. Separate intentional behavior changes from regressions.

Disprover results: `Disproved`, `Not disproved`, `Needs runtime evidence`, or `No result`. Retry `No result` once with a fresh disprover. If a claim is narrowed, assign a fresh disprover to the narrowed claim before treating it as narrowed.

## Behavior Change Authority

Intentional behavior changes are not regressions, but they still require explicit classification. A change is not intentional merely because it is cleaner, simpler, or preferable.

For each behavior change, state old behavior, new behavior, observer, explicit justification, and docs/tests/snapshots/IFA artifacts that must change.

## Output

Produce headings in this order:

1. `Scope and Observation Surfaces`
2. `Baseline Behavior Contract`
3. `Proposed Behavior Contract`
4. `Candidate Regression Claims`
5. `Disproved Concerns`
6. `Unresolved Claims`
7. `Surviving Regression Risks`
8. `Intentional Behavior Changes`
9. `Coverage and Ratchets`
10. `Verdict`

For each candidate claim include claim, surface, plausibility reason, baseline evidence, proposed evidence, unique disprover label, disprover result, and final classification.

Final classification MUST be one of `Disproved`, `Narrowed after re-disproof`, `Unresolved after disprover failure`, `Needs runtime evidence`, or `Surviving regression risk`.

For each surviving risk include severity `High`, `Medium`, or `Low`; regression; observer; exact `file:line` evidence; why the disprover could not kill it; smallest upstream fix; and regression ratchet or verification.

Say `No material regression findings.` when no risks survive. Say `None.` for empty unresolved and intentional-change categories.

Verdict MUST be exactly `No material behavioral regression risk identified`, `Regressions likely`, or `Cannot conclude`, justified in 2-5 sentences. Use `Cannot conclude` when any material claim is unresolved or needs runtime evidence unless proven immaterial.

## Hard Rules

You MUST enumerate candidate regressions before findings, freeze each claim set before disprover work, assign one disprover per claim, attempt refutation for every claim, treat preserved behavior as evidence-backed contract, and report both baseline and proposed evidence.

You MUST NOT ignore relevant observation surfaces, let claims survive without refutation, settle narrowed claims without fresh disprover work, report regressions without baseline/changed evidence unless the baseline is missing, treat cleaner behavior as automatically intentional, collapse into a generic design review, plan review, or security audit when the real issue belongs to another skill, or implement fixes unless explicitly asked.

Severity labels in this skill are local to this skill and are not directly comparable to `$doublecheck` or `$check-design`.
