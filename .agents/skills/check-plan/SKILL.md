---
name: check-plan
description: "Use when you want to verify a plan is implementation-ready. Run this after blueprint to confirm two independent engineers could implement equivalently."
---

# Check Plan

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Perform a read-only implementation-readiness review.

Central test: would two independent engineers, using only the plan, `docs/IFA.md`, cited docs, and the current codebase, implement behaviorally equivalent and architecturally compatible solutions?

If they could reasonably diverge on a material concern, the plan is not implementation-ready and you MUST flag the underspecification.

Material concerns:

- correctness
- user-visible, script-visible, public API, harness, or downstream-consumer behavior
- persisted state, rendering, snapshots, errors, approvals, queueing, dispatch, continuation, or resume semantics
- security, authority, proof-carrying state, state-machine transitions, lifecycle, ownership, equality, clone, or move semantics when correctness-relevant
- RUST_DESIGN conformance

Do not require convergence on private helper names, local decomposition, file layout, or test organization unless the change materially affects a planning concern.

This skill may improve plans. It MUST NOT implement them or change code unless explicitly asked.

If the request is regression-heavy or regression-only, defer to `$check-regressions`.

## Inputs

Treat attached file references, pasted paths, pasted plan text, and trailing prompt text as the plan input.

Valid forms:

- `$check-plan @./.plans/<plan>.md`
- `$check-plan ./.plans/<plan>.md`
- `$check-plan` followed by pasted plan text

Rules:

- Read referenced plan files before review.
- If multiple plans are referenced, review all and report conflicts.
- If no plan is provided, stop and ask for a path or pasted text.
- Do not guess the plan.

## Required Evidence

Before producing findings, you MUST:

1. Read `docs/IFA.md` in full.
2. Read the plan in full.
3. Read docs, artifacts, and repository paths the plan cites as constraints or touched artifacts.
4. Verify each plan-named path exists exactly where named.
5. Trace materially affected codepaths.
6. Check every non-trivial plan claim against real code.
7. Cite `file:line` for each load-bearing claim.

A codepath is materially affected when the plan changes or depends on its inputs, outputs, persisted representation, authority/proof ownership, state transitions, approvals, queueing, dispatch, continuation, resume, errors, rendering, snapshots, public APIs, scripts, harnesses, or downstream consumers.

Do not expand to adjacent systems unless the current codebase shows concrete dependency through dataflow, control flow, persistence, rendering, approval, queueing, dispatch, or authority transfer.

When the plan concerns tool execution, security boundaries, approvals, queueing, planning, execution state, or harness behavior, also read:

- `SECURITY.md`
- `docs/PARALLEL_TOOL_EXECUTION.md`

## Scope And Authority

Under-flagging material implementation risks is worse than over-flagging them. Over-reporting immaterial issues is scope drift.

A finding is valid only when it affects a material concern or identifies a stale, missing, moved, renamed, deleted, or superseded reference used by the plan.

Architectural refactors, type/model changes, and redesigns are allowed when they materially improve correctness, convergence, RUST_DESIGN conformance, authority preservation, state-machine integrity, persistence compatibility, maintainability of an affected codepath, or implementation risk.

A recommended expansion is in scope only when it is required for the plan objective, preserves behavior while improving affected architecture, fixes a concrete RUST_DESIGN/proof/authority/state-machine defect, prevents a concrete regression, or gives a narrower safer implementation of the same intended behavior.

Do not require unrelated cleanup, speculative future-proofing, generalized framework work, new feature scope, new product behavior, new user-visible semantics, new persistence formats, or new downstream contracts unless the plan or a cited design constraint requires them.

Useful but non-required architecture ideas belong in `Optional Architecture Notes`, not `Required Revisions`.

## Behavior And Architecture Rules

The reviewer MUST NOT silently authorize new behavior.

Classify every behavior change as exactly one:

- intentional behavior change required by the plan
- intentional behavior change required by a cited design constraint
- proposed behavior change requiring author approval
- possible unintentional behavior change or regression

A behavior change is not acceptable merely because it is cleaner, simpler, more maintainable, or architecturally preferable.

For every behavior-changing finding, state old behavior, new behavior, affected material concern, explicit justification, and whether author approval is required.

Behavior-preserving refactors and redesigns MAY be required when the plan would otherwise duplicate authority/proof state, recompute reviewed authority from weaker inputs, weaken typestate or ownership, introduce pseudo-optional state, rely on bare booleans for policy/authority/lifecycle, create ambiguous states, obscure state-machine auditability, split behavior across callers, force downstream inference from incidental structure, or violate RUST_DESIGN.

For every required refactor or redesign, state the concrete defect, replacement architecture, unchanged behavior, changed behavior if any, why it is required rather than preferable, and code/doc evidence.

## Checks

Check materially affected codepaths for:

- end-to-end correctness from input boundary through planning, approvals, persistence, queueing, execution, rendering, and errors
- reference accuracy for all named files, docs, READMEs, IFA artifacts, plans, and repository paths
- interface precision for changed types, state carriers, ownership boundaries, enums, proofs, queue entries, and UI/rendering shapes
- state-machine integrity for ordering, transitions, continuation/resume, failures, retries, queueing, and dispatch
- behavioral preservation unless the plan explicitly changes behavior
- shared-state and side-effect risks involving runtime state, persisted state, queues, config, journals, approvals, caches, globals, concurrency, I/O, resource lifecycles, panics, aborts, and cleanup
- RUST_DESIGN violations: uninhabited states, pseudo-optional modeling, bare booleans, wildcard policy variants, weakened typestate, proof loss, or recomputation from weaker authority inputs
- Hauberk-specific risks: proof-carrying types, reviewed authority, approval continuation/re-entry, snapshot/UI/rendering consumers, persistence readers, inline audit trails, and IFA updates

## Severity And Classifications

Severity:

- `High`: blocks readiness; risk of incorrect behavior, authority loss, state-machine corruption, persistence incompatibility, significant downstream breakage, or clear RUST_DESIGN violation.
- `Medium`: usually blocks readiness; material underspecification or risk of divergent implementations, behavioral drift, compatibility issues, or architectural defects.
- `Low`: non-blocking by default; bounded issue, minor stale reference, optional clarification, or non-critical improvement.

A Low finding MUST NOT appear in `Required Revisions` unless it affects a material concern.

Each finding MUST use one primary classification:

- `implementation clarification`
- `behavior-preserving architectural refactor`
- `behavior-preserving architectural redesign`
- `justified behavior change`
- `proposed behavior change requiring author approval`
- `possible unintentional behavior change or regression`
- `stale or incorrect reference`
- `insufficient evidence`

Use `justified behavior change` only when required by the plan or a cited design constraint.

## Output

Produce headings in this order:

1. `Findings`
2. `Behavioral Changes`
3. `Convergence Verdict`
4. `Required Revisions`
5. `Optional Architecture Notes` only when useful

For each finding include severity, classification, gap or ambiguity, why it matters, exact `file:line` citations, and concrete resolution. For behavior-changing findings also include old behavior, new behavior, affected material concern, plan justification, and approval requirement.

`Behavioral Changes` MUST list intentional plan changes, changes required by design constraints, proposed changes requiring approval, possible unintentional changes/regressions, and preserved material concerns. Use `None` for empty categories.

`Convergence Verdict` MUST be exactly one of `Implementation-ready`, `Close but not implementation-ready`, or `Not implementation-ready`, justified in 2-6 sentences. Use `Implementation-ready` only when the two-engineer test passes for all material concerns and no High or Medium blockers remain.

`Required Revisions` MUST include only implementation-directive revisions required to resolve blocking findings. Do not put optional cleanup, speculative work, unrelated refactors, or merely useful architecture ideas there. Required revisions MUST NOT introduce behavior changes unless already explicit in the plan or required by cited design authority.

## Hard Rules

You MUST be thorough, cite exact `file:line`, verify plan claims against real code, apply the two-engineer convergence test, distinguish blockers from optional improvements, classify every behavior change, preserve architectural judgment, avoid unnecessary scope drift, and propose concrete resolutions.

You MUST NOT implement the plan, change code unless asked, accept claims without tracing, hand-wave with “seems fine” or “should work”, authorize behavior changes because they are cleaner, inflate scope with unrelated/speculative work, or require convergence on private details that do not affect material concerns.
