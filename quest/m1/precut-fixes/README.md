# Pre-cut correctness fixes

## Goal

Every defect the 2026-11-17 pre-cut correctness audit found is fixed on `main` before the `0.22.0` cut,
so the cut publishes none of them.

## Plan

The audit read everything that changed from `release`
([ca3b801a6c](https://github.com/moq-dev/moq/commit/ca3b801a6c)) to `main`
([ec5a19540f](https://github.com/moq-dev/moq/commit/ec5a19540f)) for bugs,
hostile input, performance, fragile lifecycles, and unproven claims, with
eighteen read-only reviewers by area. API shape and whether a feature should
exist were out of scope: the 2026-11-16 audit and the
[API shape review](/quest/m3/release/api-review.md) own them. So was code an
open m2 quest replaces (the old lite-07 route rule, YIELDED continuations,
lite-07 sequence addressing, socket endpoints, moq-e2ee).

Decided 2026-11-17 (maintainer, [moq-dev/moq#9010](https://github.com/moq-dev/moq/pull/9010#issuecomment-6959731587)): ✅ every finding gates the cut, including those `release` already ships,
so they all live in this epic, which the cut requires (rejected: gating only
what `release` lacks, the agent's /quest-afk recommendation; gating none).
The one exception: ✅ [ts-step-damage](/quest/m1/ts-step-damage.md) is a
standalone low-priority m1 quest outside the gate, since it was read from
source with no field report (rejected: keeping it in the gate; deleting it).
✅ Every quest the audit added stays in m1 (rejected: only this epic in m1).

Each child reproduces its finding before fixing it: most were read from
source, not run. The reviewers' reports were scratch and are not kept; each
quest carries what its implementer needs.

## Related

- [Feature review](/quest/m3/release/feature-review.md) - the delta pass after m2 judges what m2 lands, which this audit skipped
