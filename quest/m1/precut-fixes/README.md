# Pre-cut correctness fixes

## Goal

Every regression and defect in newly shipped code that the 2026-11-17
pre-cut correctness audit found is fixed on `main` before the `0.22.0` cut,
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

Recommended 2026-11-17 (agent, under /quest-afk, for the maintainer's
review): ✅ a finding gates the cut when `release` does not have it, being a
regression or a defect in code the cut publishes for the first time
(rejected: gating every finding, which holds the cut on bugs `release`
already ships; gating none, which publishes regressions). The audit's
findings that `release` already has are standalone m1 quests that ship
whenever they land.

Each child reproduces its finding before fixing it: most were read from
source, not run. The reviewers' reports were scratch and are not kept; each
quest carries what its implementer needs.

## Related

- [Feature review](/quest/m3/release/feature-review.md) - the delta pass after m2 judges what m2 lands, which this audit skipped
