---
name: check-docs
description: "Use when you want to verify docs match current code behavior. Run this after blueprint or code changes to catch stale references and misleading claims."
---

# Check Docs

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Perform a read-only documentation accuracy audit grounded in the current repository.

Central test: would a user, contributor, implementer, or downstream consumer relying on the docs be materially misled about current code, behavior, commands, architecture, invariants, or security posture?

Material doc concerns:

- broken/stale paths
- stale symbols/APIs/config/tool names/commands/flags/schemas
- behavior claims that no longer match code/tests/snapshots/output
- canonical examples that violate `docs/IFA.md`
- incorrect security/authority/persistence/approval/state-machine claims
- missing docs for major public APIs or authority boundaries
- draft docs with misleading status

Treat doc claims as untrusted until code, file layout, command evidence, tests, or a proven absence search confirms them.

## Inputs

Accept a specific doc set or whole-repo docs sweep. If the user does not narrow scope, audit the whole documentation surface.

## Required Evidence

Before findings, read:

- every doc in scope
- `docs/IFA.md` in full before judging canonical Rust snippets or architecture claims
- codepaths, symbols, config schemas, tool registries, tests, snapshots, and IFA artifacts needed to verify doc claims

Cite doc `file:line` and code `file:line`, or provide a reproducible no-match proof.

## Scope Discipline

Under-flagging material doc drift is worse than over-flagging it. Over-reporting immaterial prose issues is scope drift.

A finding is valid only when docs could materially mislead a reader or when a referenced path, link, symbol, command, schema, or artifact is stale.

Do not report grammar, tone, formatting, or editorial preference unless the user asks for editing.

## Checks

Inventory docs in scope, including `docs/**/*.md`, crate `README.md` files, and repo-root docs such as `README.md`, `SECURITY.md`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, and `ledger.md`.

Check references to paths, modules, symbols, fenced code, links, anchors, CLI commands/flags, config keys, schema names, tool names, registry entries, and IFA entries.

Verify Rust snippets according to their declared mode, including `ignore`, `compile_fail`, and `no_run`.

Cross-check behavior claims against canonical codepaths, command output, snapshots, or tests that encode the current contract.

Classify `DRAFT_` docs as `promote` (to be promoted) if implemented, archive if obsolete, or retain as draft if partially implemented.

Disprove candidate findings before reporting them.

## Behavior Change Authority

Docs may describe changed behavior only when code supports it or the doc clearly marks it as planned/proposed.

Report doc-code drift when docs present unsupported behavior as current. Report documentation gaps when material behavior changes lack public API, user-visible, persistence, security UX, or authority-semantics documentation.

## Severity

Use `Critical`, `High`, `Medium`, or `Low`.

`Critical` means unsafe, destructive, or severely incompatible guidance. `High` means likely incorrect implementation, user-facing breakage, security misunderstanding, or downstream drift. `Medium` means material mismatch or missing important docs. `Low` means bounded stale reference or limited gap.

## Output

Produce headings in this order: `Summary`, `Findings`, `Swept Clean`, `Open Questions`, `Actionable Remediation`.

`Summary` lists docs scanned, reference types checked, and findings by severity.

For each finding include severity, doc `file:line`, code `file:line` or no-match proof, stale/misleading claim, current truth, and suggested fix.

`Swept Clean` lists material concerns checked and disproved. `Open Questions` lists only user-intent blockers; say `None.` when empty. `Actionable Remediation` groups next steps by doc or subsystem.

## Hard Rules

You MUST stay read-only unless asked for edits, report only evidence-backed doc drift, cite the doc claim and current truth, verify behavior claims against evidence, and respect RUST_DESIGN boundary-local exceptions.

You MUST NOT treat resolving paths as proof of accuracy, mislabel allowed boundary examples as core violations, report unsupported speculation, or inflate findings with immaterial prose issues.
