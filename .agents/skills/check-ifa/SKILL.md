---
name: check-ifa
description: "Determine which `ifa/*.toml` artifacts require changes for a code change, plan, architectural decision, or boundary-moving refactor, and draft concrete updates. Use when invariants, authority boundaries, proof ownership, move semantics, parametricity, classification, or persistence semantics may drift."
---

# Check IFA

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Determine whether a scoped change requires updates to Hauberk's IFA artifacts and, when required, draft exact artifact deltas.

Central test: would leaving IFA artifacts unchanged let future implementers, reviewers, or tools misunderstand current invariants, authority boundaries, proof ownership, move semantics, parametricity, classification, or persistence meaning?

Material IFA concerns: domain guarantees, typestate proofs, proof ownership, trust boundaries, capability scope, authority owners, generic constraints, trait-bound discipline, move-only transitions, consumed-state flows, policy/mechanism placement, data classification, and user/downstream behavior enabled by stale invariants.

Treat IFA updates as requirements that must be satisfied in the same change, not deferred to follow-up cleanup.

## Inputs

Accept staged diffs, unstaged diffs, named files/modules/crates, or plans/design artifacts proposing architectural change. If scope is ambiguous, ask for the missing target.

## Required Evidence

Before conclusions, read:

- `docs/IFA.md` in full
- `docs/IFA_CONFORMANCE_RULES.md`
- `ifa/README.md`
- scoped code, tests, docs, and existing IFA entries that govern the change

Cite `file:line` for evidence justifying each artifact decision.

## Scope Discipline

Under-flagging material IFA drift is worse than over-flagging it. Artifact churn with no invariant, authority, proof, move, parametricity, classification, persistence, or behavior consequence is scope drift.

A required update is valid only when stale IFA content could misstate a material IFA concern.

## Checks

Evaluate all six artifacts explicitly:

- `ifa/invariant_registry.toml`
- `ifa/authority_boundary_map.toml`
- `ifa/parametricity_rules.toml`
- `ifa/move_semantics_rules.toml`
- `ifa/dry_proof_map.toml`
- `ifa/classification_map.toml`

For each artifact, decide exactly one: `Update required`, `Confirmed unchanged`, or `Needs user intent`.

For every required update, provide a TOML snippet, the corresponding code or plan change that necessitates the update, reason for change, drifted invariant, boundary, proof, move-semantics, parametricity, or classification, and the concrete behavioral or authority regression that stale artifacts could permit.

Run `just ifa-check` when repo state allows verification.

## Behavior Change Authority

IFA edits may document changed semantics only when the scoped change actually changes those semantics or a cited design constraint requires them.

Do not use IFA wording to silently authorize new behavior. If an IFA update implies behavior change, state old behavior, new behavior, observer, and explicit justification.

## Output

Produce headings in this order: `Summary`, `Required Updates`, `Regression If Omitted`, `Unchanged Artifacts`, `Open Questions`, `Verification`, `Commit Chunk`.

`Summary` lists artifact decisions. `Required Updates` includes snippets, rationale, and evidence. `Regression If Omitted` states concrete drift or bugs per update. `Unchanged Artifacts` gives one-line evidence-backed reasons. `Open Questions` lists only unresolved intent; say `None.` when empty. `Verification` reports `just ifa-check` status. `Commit Chunk` gives a concise IFA-focused commit/PR paragraph.

## Hard Rules

You MUST check all six artifacts, tie stale artifact risk to concrete drift, preserve proof ownership and policy/mechanism splits unless RUST_DESIGN forces redesign, and require same-change IFA updates when material semantics change.

You MUST NOT hand-wave with “probably no IFA change”, defer required IFA work unless the user accepts risk, draft artifact churn without material consequence, or silently authorize behavior changes through IFA wording.
