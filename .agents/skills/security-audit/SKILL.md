---
name: security-audit
description: "Repository-grounded application security audit identifying concrete vulnerabilities, exploit paths, trust-boundary failures, insecure defaults, and missing hardening controls. Trigger only when the user explicitly requests a security audit, AppSec review, vulnerability assessment, penetration-test-style or red-team-style code review, security findings report, or exploit-oriented review of a security-sensitive change."
---

# Security Audit

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Perform an evidence-based application security audit.

Central test: can a named attacker persona use a concrete, reachable path in the current implementation to cross a trust boundary and cause confidentiality, integrity, availability, or authorization impact?

A valid security finding has persona, capability, prerequisites, trust boundary crossed, intended control, why the control fails or may fail, concrete impact, and exact `file:line` evidence.

Prefer fewer high-confidence findings over many speculative concerns.

Use only when the user explicitly requests a security audit, AppSec review, vulnerability assessment, penetration-test-style review, red-team-style review, security findings report, exploit-finding request, or exploit-oriented review of a security-sensitive change.

For security-visible UX or behavior drift without exploit framing, prefer `$check-regressions`.

## Inputs

If scope is unclear, identify repo root and user-named paths. Ask only when missing deployment, trust, or exposure assumptions materially change severity and cannot be stated as assumptions.

## Required Evidence

Before findings, read:

- `AGENTS.md`
- `docs/IFA.md`
- `SECURITY.md`
- `docs/SECURITY_THREAT_MODEL.md`
- most recent adversarial or hardening review doc in `docs/`
- in-scope code, tests, docs, and IFA artifacts
- `security-audit-report.md` at repo root or another user-named report path when present

Use authority docs for intended behavior and implementation evidence for actual behavior.

## Prior Audit Handling

Treat prior reports as known-path maps, not truth.

For prior findings, re-check the current tree, mark fixed issues as `Previously reported, now remediated`, mark still-present issues as `Previously reported, still open`, avoid retracing unchanged covered codepaths, and report as novel only when current evidence introduces a new persona, boundary, codepath, precondition set, or impact path.

Do not re-report stale findings as novel.

## Scope Discipline

Under-reporting concrete security issues is worse than over-reporting them. Reporting issues without plausible exploit paths is scope drift.

Do not report style issues, hardening preferences, or generic best-practice gaps as security findings unless they create concrete security impact.

Separate confirmed vulnerabilities from residual risk, missing hardening, and testing gaps.

## Evidence And Personas

Finding status MUST be exactly one of:

- `Observed`: directly established from current source or supplied artifacts
- `Inferred`: derived from source but not directly executed or observed
- `Needs dynamic validation`: static analysis cannot settle runtime, kernel, timing, binary, integration, or platform behavior

Call a vulnerability proven only when the vulnerable condition is directly observable and the exploit path is concrete from code flow, or supplied evidence proves the vulnerability and the current source tree still contains the vulnerable code.

Use one primary persona per finding: `MaliciousRepoAuthor`, `SocialEngineerWithUserApproval`, `SameUserLocalMalware`, `SandboxedChildProcess`, `HostileRemoteContent`, or `CoTenantLocalUser`.

## Audit Checks

Map in-scope attack surface across prompt/instruction ingress, slash-command and skill invocation, approval/policy mediation, approval prompts/warnings/auth/redaction, tool argument validation, shell canonicalization and blacklist boundaries, sandbox/process hardening, persistence/recovery, display sanitization, platform confinement, network egress/redirects, and tenant/user isolation.

Prioritize authentication, authorization, secrets, declassification, parsing, boundary translation, filesystem access, path resolution, subprocesses, sandboxing, policy bypass, persistence, recovery, encryption, plaintext fallback, insecure defaults, fail-open behavior, and dangerous overrides.

For each suspected issue, trace attacker input or capability to impact, identify limiting controls, and discard or downgrade claims blocked by real guards.

## Remediation And Behavior

Every remediation MUST include narrow fix shape, why it closes the issue, regression risk, behavior preserved, tests to add/update, and residual risk.

Prefer fixes that strengthen type/authority boundaries, tighten policy without unrelated denial changes, collapse ambiguity at the immediate boundary, replace ad hoc runtime checks with proof-carrying or typestate transitions, and add a ratchet test.

Do not propose any of these anti-pattern fixes:

- broad deny-all changes that silently break legitimate workflows
- configuration switches that merely hide the bug
- fail-open fallback paths
- vague "sanitize more" guidance without the exact boundary and callsite
- new bare booleans in repo-defined surfaces
- new `Option<T>` in repo-defined surfaces
- new wildcard bypass variants such as `Any`, `All`, `Unrestricted`, or `AllowAll`
- `unwrap`, `expect`, `unreachable!`, or lock-based domain coordination as a substitute for correct modeling

`docs/IFA.md` is the leading authority. Prefer consequence-first enums, typestate, capability tokens, sealed phases, and boundary-local ambiguity collapse. If a remediation would conflict with the design authority, state that explicitly and propose the design-compatible shape.

Security fixes may change behavior only to the minimum extent required to close the issue or satisfy cited design authority. For behavior-changing remediation, state old behavior, new behavior, observer, why required, and ratifying test/doc.

## POC Guidance

For proven or near-proven findings, provide controlled validation with objective, preconditions, proof path, local validation approach, expected vulnerable signal, expected fixed signal, and safety boundary.

Prefer compile-fail tests, unit tests, hermetic integration tests, fixture-based tests, or deterministic harness tests.

Do not execute live exploitation against external systems, add persistence, stealth, lateral movement, operational tradecraft, or public-target instructions. If validation was not executed, say so and provide a test skeleton or local reproduction recipe.

## Regression-Safety Checklist

Before proposing any fix, verify all of the following:

- the change does not alter the normal success path beyond the minimum required for security
- the change does not silently widen or narrow approvals unrelated to the issue
- the change does not introduce new hidden defaults, wildcard policies, or fail-open branches
- the change does not change persisted-state meaning without explicit migration handling
- the change does not break documented command or prompt semantics
- the change does not unintentionally alter approval prompts, warnings, redaction surfaces, or auth-visible behavior
- the change includes a ratchet test that fails on the vulnerable behavior and passes on the intended behavior

## Output

Output exactly `No findings.` only when all of the following are true: there are no novel findings; no prior findings require status reporting; and no material dynamic-validation blockers remain.

Otherwise produce headings in this order: `Scope`, `Threat Surface`, `Findings`, `Previously Reported, Now Remediated`, `Previously Reported, Still Open`, `Open Questions Requiring Dynamic Validation`, `Residual Risks and Testing Gaps`, `Prioritized Fix Plan`, `Overall Verdict`.

For each finding include severity `Critical`, `High`, `Medium`, or `Low`; status; title; persona; what failed; impact; exploitability; trust boundary crossed; intended control and failure mode; exact evidence; controlled validation plan; remediation; and regression guardrails.

Omit prior-report headings when not applicable. Keep residual risks and testing gaps separate from findings.

If the user asks for a written report, write Markdown to `security-audit-report.md` or the specified path. If that file exists, merge novel findings without erasing prior finding identities.

## Hard Rules

You MUST be evidence-first, trace codepaths end to end, label each finding with evidence status, name an attacker persona, distinguish vulnerabilities from residual risk, avoid retracing unchanged prior-report paths, and call out fail-open behavior, authority leakage, unsafe recovery, trust-boundary confusion, and missing enforcement when evidence supports them.

You MUST NOT conflate threat modeling with implementation findings, accept “handled elsewhere” without tracing, report hypothetical issues without code evidence, re-report prior findings as new, propose remediations that fail open/hide bugs/widen approvals/violate RUST_DESIGN, or pad reports with generic advice unless asked.
