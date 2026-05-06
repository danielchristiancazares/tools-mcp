---
name: commit-sweep
description: "Analyze the full current worktree and propose atomic commits with precise conventional messages and staging commands. Use when the user wants deliberate, bisect-friendly commits without leaking unrelated worktree state or hiding behavioral changes inside refactors."
---

# Commit Sweep

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Turn the full current worktree into a deliberate commit plan.

Central test: would each proposed commit be atomic, self-contained, bisect-safe, accurately messaged, and free of hidden behavior changes relative to its staged diff?

This skill proposes a commit plan first. It MUST NOT mutate git until the user approves execution.

## Material Concerns

- every dirty path is classified
- staged paths match the commit message
- behavior changes are isolated or named
- refactors do not smuggle behavior
- public API, docs, tests, IFA, snapshots, and security updates are bundled with the changes that require them
- intermediate commits do not break builds, security posture, persistence semantics, or documented behavior

## Inputs

Default to the full current worktree. `$commit-sweep` means sweep the whole worktree.

If the user narrows execution scope, still inspect the full worktree first and classify every out-of-scope path as deferred.

## Required Evidence

Before proposing commits, capture:

- `git status --short --branch`
- `git diff`
- `git diff --cached`
- `git ls-files --others --exclude-standard`

Inspect every modified, added, deleted, renamed, staged, unstaged, and untracked path.

## Scope Discipline

Under-classifying worktree state is worse than over-classifying it. Over-splitting into artificially small commits that create broken intermediate states is scope drift.

Every dirty path MUST be committed, hunk-split into named commits, or deferred with a path-specific reason.

Do not invent cleanup commits, message claims, or staging splits unsupported by actual hunks.

## Behavior Change Authority

Commit messages MUST NOT hide behavior changes inside `refactor`, `chore`, `test`, or `docs` wording.

Every behavior-changing commit must name the behavior delta in the body unless the header fully conveys it.

A behavior change may remain in a refactor-shaped commit only when splitting it would create an incoherent or broken intermediate commit; the body MUST call it out.

## Workflow

1. Capture full worktree state.
2. Analyze each changed path.
3. Identify behavior changes, supporting refactors, docs, tests, IFA changes, security-sensitive changes, and persistence/schema changes.
4. Group atomic commits.
5. Keep coupled implementation, tests, docs, snapshots, IFA, and security changes together.
6. Split independently meaningful changes apart without broken intermediate commits.
7. Draft conventional commits using `feat`, `fix`, `refactor`, `test`, `docs`, or `chore`.
8. Plan exact `git add <path>` or `git add -p <path>` commands.
9. Run the self-check.
10. Present the full plan before mutating git.
11. After approval, execute one commit at a time.

Never use `git add .`, `git add -A`, or `git add -u`.

## Self-Check

Before presenting the plan, act as a hostile reviewer of your own plan and emit an explicit verification block as visible output. You MUST NOT proceed to execution proposals until every item below is discharged.

- Claim grounding (per commit): every sentence in the commit message body cites a specific staged file or hunk. Delete sentences you cannot cite. Reject aspirational or "also improves…" claims with no matching hunk.
- Scope and type accuracy (per commit): `type(scope)` matches the staged paths and the actual nature of the change (no `fix` for a refactor, no `tui` scope for `engine/` paths, no `docs` type shipping code).
- Self-containment (per commit): the tree after commits 1..N is coherent — no "fixed in a later commit" build breaks, no intermediate insecure sanitization/redaction state, no partial refactor that only compiles once commit N+1 lands.
- Docs bundling (per commit): public-API or user-visible changes ship their `docs/` update in the same commit.
- IFA bundling (per commit): invariant, authority-boundary, proof, parametricity, move-semantics, or classification changes ship their `ifa/` update in the same commit.
- Security bundling (per commit): security-sensitive sanitization, redaction, normalization, or key-exposure changes are not split across commits.
- Behavior visibility (per commit): if a commit changes observable behavior, that delta is isolated or explicitly called out. Refactor commits MUST NOT smuggle in behavior changes that would be hard to bisect or explain from the message.
- Worktree coverage (plan-wide): every path from the worktree capture is classified as fully staged in one commit, hunk-split across named commits, or explicitly deferred. No path unlisted.
- Contradiction sweep (plan-wide): header vs. body, body vs. staged hunks, one commit's claim vs. another's, scope vs. staged paths.
- No-op check (plan-wide): every commit has a non-empty, meaningful staged diff after the claim-grounding pass.

## Execution After Approval

For each approved commit: stage only planned paths/hunks, inspect `git diff --cached --stat`, inspect `git diff --cached`, commit with plain `git commit`, verify `git status --short`, and verify `git log -1 --stat`.

If the index needs cleanup, explain and wait for approval before using `git reset`.

## Output

For each proposed commit include number, header, full body, exact staging commands, one-sentence rationale, and self-check summary.

Then list files remaining unstaged with path, reason, and whether they should become follow-up commits.

## Hard Rules

You MUST inspect the full worktree, classify every dirty path, present before mutating git, use precise staging commands, match messages to diffs, keep behavior changes visible, and keep GPG signing enabled by using plain `git commit`.

You MUST NOT use broad git add commands, amend unless asked, stage unrelated files, silently omit dirty paths, split changes into broken intermediate commits, or commit before approval.
