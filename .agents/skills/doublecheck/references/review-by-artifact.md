# Review by Artifact

Read only the section that matches the current review target.

## Plan or Design Doc

- Verify that each claimed codepath, state transition, and preserved behavior matches the current tree.
- Look for underspecified interfaces, ownership boundaries, enum shapes, proof carriers, and failure paths.
- Check whether two independent implementers could diverge on behavior, ordering, cleanup, or authority ownership.
- Confirm the plan explicitly names intentional behavior changes, required docs updates, and `ifa/` updates.

## Diff, Staged Change, or PR

- Compare changed and unchanged callers, not just edited lines.
- Trace removed branches, moved logic, and renamed types for lost invariants or hidden side effects.
- Check that tests prove the real behavioral claim instead of snapshotting a symptom.
- Verify docs, `ifa/`, and commit message claims still match the actual patch.

## Existing Code or Implementation

- Find the entrypoints, boundaries, owners, and persisted state before judging a local pattern.
- Treat `RUST_DESIGN` violations as real until the type boundary disproves the concern.
- Look for invalid-state reachability, proof recomputation from weaker data, and authority leakage across helpers.

## Test

- State exactly what the test claims to prove.
- Check whether the assertion proves an invariant, or only mirrors the current implementation detail.
- Look for missing negative cases, missing boundary cases, and cases where a refactor could break behavior while the test still passes.
- Verify fixtures and mocks preserve the real authority, persistence, and error semantics under test.

## Commit Message Versus Staged Diff

- Verify every meaningful claim in the commit message is supported by the staged diff.
- Flag scope drift when the diff changes behavior the message does not mention.
- Flag overclaiming when the message says an invariant, cleanup, or behavior is fixed but the diff does not prove it.
- Check whether required docs or `ifa/` updates are missing from the staged set.

## Prior Review or Your Own Last Output

- Re-check every major claim against the current artifact and codebase.
- Look for skipped callers, skipped failure paths, or severity that does not match the demonstrated impact.
- Flag unsupported certainty, missing citations, or advice that fixes a symptom instead of the root cause.
- Prefer new evidence over consistency with the earlier review.
