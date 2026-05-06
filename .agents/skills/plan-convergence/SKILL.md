---
name: plan-convergence
description: "Orchestrate a bounded multi-agent convergence review for Hauberk plans, drafts, architectural proposals, or implementation handoffs. Use when the user explicitly asks to spawn agents, delegate to parallel agents, run a four-agent review, converge on a plan, assign architectural/security/implementation/adversarial roles, or decide how to revise a plan without scope creep."
---

# Plan Convergence

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Run a bounded, evidence-grounded, multi-agent review that converges on implementation-ready plan revisions.

The central test is whether independent engineers, using only the plan, `docs/IFA.md`, cited docs, and current code, would implement behaviorally equivalent and architecturally compatible changes.

This skill reviews and may patch plan artifacts when explicitly requested. It MUST NOT implement the plan's code changes unless the user separately asks for implementation.

## Preconditions

Spawn subagents only when the user explicitly asks for subagents, delegation, or parallel agents. If subagents are unavailable, run the same roles locally and state that limitation.

Before spawning or reviewing, read:

- `docs/IFA.md` in full
- the referenced plan, draft, or proposal in full
- repo instructions already provided by the harness
- docs, code, tests, and IFA artifacts that the plan names or materially affects
- `SECURITY.md` and `docs/PARALLEL_TOOL_EXECUTION.md` when tool execution, approvals, sandboxing, security boundaries, or harness behavior are involved

Treat drafts under `docs/DRAFT_*.md` as proposed future work, not evidence of current implementation.

## Agent Model

Use four isolated first-pass agents. Do not let them coordinate before they return; independence is the signal.

1. **Architect**
   - Tests design convergence, ownership, state boundaries, typestate, API shape, and RUST_DESIGN fit.
   - Must identify exact owning crates/modules when the plan is vague.

2. **Security**
   - Tests trust boundaries, bypasses, authority leakage, fail-closed behavior, capability-token design, and abuse paths.
   - Must distinguish covered threats from explicitly out-of-scope threats.

3. **Implementation Engineer**
   - Tests the smallest implementable slice, call paths, dependency direction, exact mutation points, and feasible verification.
   - Must identify stale paths, missing dependencies, and overbroad acceptance criteria.

4. **Adversarial Regression Sentinel**
   - Challenges preservation claims, user-visible regressions, hidden behavior changes, stale commands, invalid rollback plans, and scope creep.
   - Must classify old behavior versus proposed behavior when a change is material.

## Shared Prompt Packet

Give each agent the same task-local packet:

```text
You are one role in a four-agent isolated convergence review. Work read-only unless explicitly told otherwise.

Target: <plan or draft path>
User goal: <one sentence>
Role: <Architect | Security | Implementation Engineer | Adversarial Regression Sentinel>

Required grounding:
- Read docs/IFA.md in full.
- Read the target plan/draft in full.
- Verify current-state claims against code/docs/tests/IFA artifacts.
- Cite repo-relative file:line evidence for load-bearing claims.

Output:
- Blocking issues, if any.
- Required plan revisions.
- Scope boundaries and overclaim risks.
- Concrete tests or verification needed.
- No code implementation.
```

Add role-specific bullets from `Agent Model`. Do not pass one agent's conclusions to another during the first pass.

## Orchestration Workflow

1. **Prepare ground truth**
   - Read required evidence locally first.
   - Build a concise shared packet: target path, user goal, relevant hard constraints, and likely affected domains.

2. **Spawn first pass**
   - Spawn all four agents in parallel when possible.
   - Prefer read-only `explorer` agents for review and convergence tasks.
   - Do not duplicate their tasks locally while they run; gather non-overlapping context instead.

3. **Synthesize centrally**
   - Make a convergence ledger:
     - unanimous findings
     - majority findings
     - role-specific findings
     - direct conflicts
     - stale references or command errors
   - Treat agreement between independent roles as strong evidence, not automatic truth.

4. **Apply tie-breakers**
   - `docs/IFA.md` wins over plan text.
   - Current code and tests win over draft claims.
   - Security and authority-boundary defects with concrete bypass paths are blocking.
   - Behavior preservation claims must be backed by current call paths and regression tests.
   - Prefer the smallest revision that satisfies the user goal, RUST_DESIGN, security posture, and current behavior constraints.
   - Do not add feature flags, compatibility shims, generic future surfaces, or speculative framework work unless explicitly required.

5. **Use follow-up agents only for unresolved material conflicts**
   - Reuse an existing agent for a related clarification.
   - Spawn a new agent only for a distinct, bounded question that cannot be resolved from evidence already gathered.
   - Never spawn agents merely to make the process look more thorough.

6. **Patch only when asked**
   - If the user requested patching, edit only the plan/draft/review artifact unless they separately requested implementation.
   - Keep revisions scoped to required convergence fixes.
   - Preserve unrelated dirty worktree changes.

## Scratchpad Rules

Do not use an agent-shared scratchpad before the first pass. Shared scratchpads collapse independence and can hide false consensus.

The parent orchestrator may keep a private synthesis scratchpad in the plan, notes, or response. If a second round is needed, send only the exact conflict and the minimum evidence required to answer it.

## Output Format

For a read-only convergence run, respond with:

```md
**Result**
<implementation-readiness verdict>

**Convergence**
- <points all or most roles agreed on>

**Tie-Break Decisions**
- <final decisions and why>

**Required Revisions**
- <minimal plan changes>

**Out Of Scope**
- <explicitly excluded work>

**Follow-Up**
<whether another agent round is needed>
```

For a patched plan, also include:

- files changed
- summary of plan revisions
- verification performed, or why none was run

## Hard Rules

You MUST keep the workflow bounded to plan convergence. You MUST cite current-state evidence for material claims. You MUST distinguish implementation requirements from optional architecture notes. You MUST close subagents when their results are no longer needed.

You MUST NOT let agents negotiate with each other before independent first-pass results. You MUST NOT treat draft documents as current-state proof. You MUST NOT invent future policy surfaces, generic permits, compatibility toggles, or rollback feature flags to make a plan seem easier. You MUST NOT broaden into implementation unless explicitly asked.
