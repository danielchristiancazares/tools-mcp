---
name: brainstorm
description: "Grounded tail-sampling brainstorm for Hauberk architectural and capability ideas. Use when the user asks for novel, codebase-aware ideas for Hauberk itself, agentic coding harness design, harness architecture, capability evolution, negative-space exploration, or a follow-up intersection brainstorm using axes from a prior brainstorm run."
---

# Brainstorm

## Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in BCP 14 [RFC2119] [RFC8174] when, and only when, they appear in all capitals, as shown here.

## Purpose

Generate high-novelty, high-relevance architectural and capability ideas for Hauberk itself, the agentic coding harness. The output MUST be grounded in the current repository and MUST avoid generic AI-agent wishlist items.

This is a read-only ideation workflow. Do not edit files unless the user separately asks for implementation or documentation changes.

## Modes

Use `Initial Mode` when the user asks for a first-pass brainstorm, tail-sampled distribution, or new axes.

Use `Intersection Follow-Up Mode` when the user asks for follow-up ideas at seams between axes, supplies prior axes, references a prior brainstorm, or asks to run immediately after a brainstorm.

If a prior brainstorm handoff block is available in the conversation, use it to avoid repeating Step 1 exploration. Treat the handoff as a cache, not as authority: if it is missing citations, stale, internally inconsistent, or insufficient for a load-bearing claim, re-open the relevant files.

## Required Grounding

Before generating ideas, build an accurate current-state model from the repository.

At minimum, read:

- `docs/IFA.md` in full.
- The top-level `README.md`.
- Current architecture docs and crate `README.md` files relevant to harness behavior.
- `SECURITY.md` and `docs/PARALLEL_TOOL_EXECUTION.md` when sandboxing, approvals, tool execution, or security behavior is relevant.
- Current docs/code that define HTTP/SSE transport, sandboxing, provider adapters, tool dispatch, session state, redaction, key handling, permits, and capability boundaries.

While reading, distinguish:

- Stable implemented behavior.
- TODOs or planned work.
- Deliberately rejected approaches.
- Draft-only claims, especially under `docs/DRAFT_*.md`.

Use compact `file:line` citations for load-bearing current-state claims.

## Initial Mode Workflow

### Step 1: Ground Truth

Produce a compact summary of what Hauberk currently is and how it is shaped.

Cover:

- Major modules: HTTP/SSE transport, sandbox layer, provider adapters for Anthropic/OpenAI/Gemini, tool dispatch, and session state.
- Security-relevant surfaces: redaction, sandboxing, permit/capability model, provider API-key handling, persistence protection, and approval boundaries.
- Explicit design principles: Invariant-First Architecture, strict Rust type modeling, provider mechanism versus caller policy, YAGNI crate policy when present, handrolled transports when present, and no-backwards-compatibility rules.
- Feature status: stable, TODO/planned, draft-only, and rejected.

This summary is the substrate for every subsequent idea. If an idea is not relevant to this summary, discard it.

### Step 2: Axes

Enumerate 6-8 orthogonal axes that distinguish agentic harness designs from each other.

These axes are not feature categories. They are design dimensions that experienced harness designers would argue about, such as where authority lives, how proofs cross boundaries, or what the harness considers durable state.

Do not reuse these example axes verbatim:

- Trust topology between harness and model.
- Where the boundary between deterministic and learned components sits.
- How the harness represents partial work to itself across turns.

### Step 3: Negative Space

Exclude ideas that reduce to these, even in disguise:

- Generic "add MCP support for X".
- RAG over the codebase as a feature.
- Multi-agent orchestration where N copies of the model talk to each other.
- "Memory" implemented as a vector store of past turns.
- Sub-agent spawning for parallel task decomposition.
- Plan/execute/reflect loops as the top-level architecture.
- Cost/latency dashboards.
- Anything whose pitch is "we use $LATEST_MODEL".

If an idea fails this filter, cut it and generate a replacement.

### Step 4: Distribution

Generate exactly 12 candidate ideas sampled from the tail (i.e., the low-probability, non-obvious region of the idea distribution).

For each idea, include:

- `name`
- `description`: exactly 2 sentences.
- `novel axis position`: which axis it occupies a novel position on.
- `Hauberk subsystem`: which existing subsystem it would touch or replace.
- `prior consideration probability`: estimated probability that an experienced harness designer would have already considered it, calibrated between `0.02` and `0.40`.
- `hard part`: one line explaining why this is hard or what would break.

Every final idea MUST have `prior consideration probability < 0.15`. If that estimate is not honest, discard the idea and regenerate.

Include 1-2 ideas marked `[possibly bad]` that seem too weird, risky, or normally filtered out. Include them because tail sampling should surface ideas the model would otherwise suppress.

### Step 5: Coherence Check

After the list, identify:

- The 2 ideas that would most change what Hauberk fundamentally is, not just what it does well. Explain what assumption each breaks about agentic harnesses.
- The 1 idea that most directly leverages something Hauberk already has that other harnesses likely do not, such as handrolled transports, IFA discipline, or its security posture.
- Any idea that is one of the excluded ideas in disguise. Cut it honestly, explain the cut, and regenerate until the final list has 12 non-excluded ideas.

### Step 6: Self-Audit

End with:

- Which axes generated zero ideas, and whether that is because the axis is genuinely barren or because the ideation flinched.
- Whether the probability distribution is suspiciously smooth, which suggests performance of tail sampling rather than estimation.
- One idea considered and discarded, with the reason.

### Step 7: Handoff Block

End the initial brainstorm with a compact handoff block for immediate follow-up runs.

The handoff block MUST include:

- `ground_truth_cache`: a compressed Step 1 summary with citations.
- `axes`: the exact final axes, lettered or numbered.
- `negative_space`: the exclusion list used.
- `trust_notes`: any repository-specific trust assumptions or architectural constraints that follow-up runs must preserve.

Use this shape:

````md
## Brainstorm Handoff

```brainstorm-handoff
ground_truth_cache:
- ...

axes:
A. ...
B. ...

negative_space:
- ...

trust_notes:
- ...
```
````

---

## Intersection Follow-Up Workflow

### Step 1: Ground Truth Cache

If a prior `Brainstorm Handoff` block or prior Step 1 summary is available, use it. Otherwise perform a fresh `Initial Mode` Step 1 exploration.

Do not treat model claims, prior idea descriptions, or uncited summaries as independent evidence. Repository files, command output, tests, docs, and IFA artifacts are the evidence.

### Step 2: Axes

Use exactly the axes provided by the prior run or by the user. Do not regenerate or rename them unless the user explicitly asks.

### Step 3: Meta-Axis Grouping

Group the axes into 2-4 meta-axes based on what they are fundamentally about. Do not reuse the user's example group labels verbatim.

Prefer intersections between meta-axes. Pairs within the same meta-axis often produce restatements.

### Step 4: Pair Selection

From all possible axis pairs, select 6-8 generative pairs.

A selected pair MUST:

- Cross meta-axis groups.
- Not already be addressed by an existing Hauberk subsystem or accepted draft.
- Contain a non-trivial design question beyond doing both axes well.

For each selected pair, write one sentence stating the design question at that intersection.

Explicitly list 2-3 rejected pairs and why they were trivial, already covered, or low-signal.

### Step 5: Intersection Ideas

For each selected pair, generate exactly 2 ideas that genuinely require both axes.

For each idea, include:

- `name`
- `description`: exactly 2 sentences.
- `axis pair`
- `collapse test`: why the idea collapses if either axis is removed.
- `Hauberk subsystem`: which existing subsystem or subsystems it touches.
- `prior consideration probability`: estimated probability that an experienced harness designer would have already considered it, calibrated between `0.02` and `0.30`.
- `hard part`: one line explaining why this is hard or what would break.

Every final idea MUST have `prior consideration probability < 0.15`. If that estimate is not honest, discard the idea and regenerate.

Include 1-2 ideas marked `[possibly bad]` that are structurally weird or risky but coherent.

### Step 6: Architectural-Mismatch Check

For each idea, ask whether it requires Hauberk to trust something it currently does not trust, or distrust something it currently treats as authoritative.

If yes, name the trust shift specifically. Flag the idea as potentially architecturally incoherent when the shift conflicts with Hauberk's invariant-first posture, including model self-attestation, unverifiable assertions as evidence, silent fallbacks, weakened authority accounting, or discarded proof boundaries.

### Step 7: Compounding Identification

Identify 1-3 pairs of ideas that compound rather than merely coexist.

Two ideas compound only when implementing both creates an emergent capability that neither creates alone. If no clear compounding pairs exist, say so.

### Step 8: Self-Audit

End with:

- Which meta-axis pair produced the most ideas, and whether that reflects genuine richness or bias.
- Whether any axis appeared in zero pairs, and why.
- Whether the collapse test was hard to apply for any idea.
- One pair that seemed desirable but could not produce ideas, with the obstacle.

## Quality Bar

Prefer strange-but-specific ideas over broadly useful generic ones. A good idea should make sense only after reading Hauberk's actual docs and code.

Do not propose an implementation plan unless the user asks. Do not hide generic ideas behind Hauberk terminology.
