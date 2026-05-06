---
name: blueprint
description: "Draft a decision-complete implementation plan artifact for a non-trivial Hauberk feature, refactor, bug fix, or architectural rewrite. Use when an engineer or agent needs an implementation handoff that fixes material decisions up front, separates behavior changes from preserved behavior, identifies affected files and invariants, and gives concrete verification without implementing the change."
---

# Blueprint

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Draft a design-time implementation artifact that another engineer or agent can implement directly.

Central test: would two independent engineers, using only the artifact, `docs/IFA.md`, cited docs, and the current codebase, implement behaviorally equivalent and architecturally compatible solutions?

If they would need to invent material behavior, architecture, state transitions, authority ownership, persistence semantics, or verification, the artifact is not decision-complete.

Material planning concerns: correctness; user/script/API/harness/downstream behavior; persisted state; rendering/snapshots/errors/approvals/queueing/dispatch/resume; security/authority/proof/state-machine/lifecycle/ownership semantics; RUST_DESIGN conformance; IFA drift.

This skill writes a plan artifact. It MUST NOT implement code unless explicitly asked after the plan is complete.

The harness runtime enforces planning at execution time via its plan mode. The artifact produced by this skill is a design-time input to that mode, not a runtime-enforced plan itself.

## Inputs

Use for non-trivial planning work: features, refactors, bug fixes, architectural rewrites, or handoffs that require concrete decisions.

Prefer review skills when the user asks for review instead of drafting: `$check-plan`, `$check-design`, `$check-regressions`, `$doublecheck`, `$security-audit`, or `$check-ifa`.

If the intended change is not identifiable from the request, supplied paths, diffs, or text, ask for the missing target.

## Required Evidence

Before drafting, read:

- `docs/IFA.md` in full
- `AGENTS.md`
- `docs/IFA_CONFORMANCE_RULES.md`
- `ifa/README.md`
- crate-local `README.md` files for implicated crates
- relevant codepaths, tests, docs, and `ifa/*.toml` artifacts
- `docs/PARALLEL_TOOL_EXECUTION.md` and `SECURITY.md` when tool execution, approvals, queueing, planning, execution state, harness behavior, sandboxing, or security-sensitive behavior is involved

Use repo-relative `file:line` anchors for load-bearing current-state claims.

## Scope And Authority

The artifact may propose refactors, type/model changes, or redesigns when they materially improve correctness, convergence, RUST_DESIGN conformance, authority preservation, state-machine integrity, persistence compatibility, implementation risk, or maintainability of the affected codepath.

Do not introduce unrelated cleanup, speculative future-proofing, generalized framework work, new feature scope, new product behavior, new user-visible semantics, new persistence formats, or new downstream contracts unless required by the user request or cited design authority.

Over-specifying private helper names, file layout, or test organization is churn unless a material planning concern depends on it.

## Behavior And Compatibility

Classify every behavior change as exactly one:

- intentional behavior change required by the user's request
- intentional behavior change required by a cited design constraint
- proposed behavior change requiring author approval
- possible unintended behavior change to avoid

For each behavior change, state old behavior, new behavior, affected material concern, why the change is required or needs approval, and tests/docs/snapshots/IFA updates that ratify it.

Hauberk plans target the final architecture. Do not propose compatibility shims, deprecation windows, old-name aliases, dual implementations, feature-flagged migrations, runtime version detection, or preserved-for-callers exports unless the user explicitly requires compatibility for a named surface.

The no-backwards-compatibility stance does not authorize unlabeled behavior change. Clean breaks MUST be named, scoped, and verified.

## Workflow

1. Classify the target as `feature`, `refactor`, `bug fix`, or `architectural rewrite`.
2. Restate the user's intent in one sentence.
3. Gather current-state truth from code, tests, docs, and IFA artifacts.
4. Define the final architecture directly.
5. Map material deltas across code, docs, tests, IFA artifacts, UI/protocol behavior, operation graph behavior, security, persistence, and downstream consumers.
6. Specify interfaces, state transitions, ownership, authority, persistence, rendering, and verification wherever implementation would otherwise require invention.
7. Ensure `docs/plans/` exists.
8. Write `docs/plans/PLAN_<TOPIC>.md` (all-caps snake-case topic, e.g., `PLAN_SESSION_PERSISTENCE`).
9. Return the artifact path and key design decisions. Respond with only the blueprint in your next message.

## Artifact Format

Use exactly these top-level headings:

```md
# <Concise Topic> Blueprint

## One-Sentence Summary
## Problem Statement
## Current State
## End State
## Behavior Changes
## Affected Files
## IFA Deltas
## UI/Protocol Impact
## Operation-Graph Impact
## Test Plan
## Out-of-Scope
## Verification
## Risks
```

Requirements:

- Use the headings exactly as shown.
- Prefer short paragraphs for summary/problem and flat bullets elsewhere.
- Use a compact table in `Affected Files` when more than three files are involved.
- Say `None.` with one sentence of proof when a heading has no changes.
- In `Test Plan`, name concrete regression ratchets for preserved behavior and intentional behavior changes.
- In `Verification`, use `just verify` as the default baseline and add `just cov` when coverage-sensitive behavior or tests are materially affected.

The artifact MUST include current behavior evidence, final architecture, affected files, docs/README updates for public or user-visible changes, exact IFA artifact decisions, behavior changes, preserved material concerns, key assumptions, test scenarios, verification commands, and out-of-scope boundaries.

Do not use vague directives such as `update as needed`, `handle edge cases`, or `wire this through` when a material decision is required.

## Hauberk Rules

`docs/IFA.md` wins when rules conflict.

UI/protocol-state and operation-graph conformance are hard gates.

Do not rely on `.unwrap()`, `.expect()`, `unreachable!()`, or fallback branches to carry proofs.

Prefer typestate, narrowing, consequence-first enums, proof-carrying values, and explicit authority boundaries over flag-guarded dual paths.

Callers, tests, docs, and `ifa/*.toml` artifacts are in scope to update when the change alters their contract.

## Hard Rules

You MUST draft a decision-complete artifact, cite load-bearing current-state claims, classify behavior changes, preserve architectural judgment, avoid unnecessary scope drift, distinguish final architecture from compatibility scaffolding, and write under `docs/plans/`.

You MUST NOT implement code unless explicitly asked after planning, plan from memory when evidence is needed, hide behavior changes inside refactors, invent new product scope, preserve old call sites merely for compatibility, or over-specify immaterial private implementation details.
