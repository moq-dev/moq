# [M] A cycle runs in phases by kind, cut on demand

## Goal

After the next cut, each release cycle runs in three phases by kind of
change: additive work, then API breaks, then wire changes. `main` can be cut
to `release` on demand at any point in the first two phases. The wire phase
is short, because `main` cannot be cut while a wire version is half changed.
A cut made while the cycle is still additive publishes patch versions; the
first cut that carries an API break or a wire change advances the shared
minor. The questline, `CONTRIBUTING.md`'s Releases and Versions, the version
check, and the cut's recurrence all say so.

## Plan

Decided 2026-11-09 (maintainer, `/quest-plan`):

- **Cadence.** ✅ Cuts on demand, in phases by kind (rejected: a weekly,
  two-weekly or monthly schedule).
- **Wire work.** ✅ A short wire phase at the end of each cycle (rejected:
  an unfinished wire version that is opt-in and carries a revision in its
  name, so `main` is always cuttable; wire work on a branch until final).
- **Versions.** ✅ Patches while the cycle is additive (rejected: a shared
  minor per cut, today's rule; one shared minor per cycle, holding API
  breaks until the wire phase ends).
- **The next cut is unchanged.** ✅ It still waits on all of m2 (rejected:
  cutting once no break is half-landed).

Why: a cut that waits for every breaking change grows large. On 2026-11-09
`main` was 3,190 commits ahead of `release`, and a deployment that follows
`release` can only take that whole. A wire change is the kind a deployed
peer must adapt to while a cluster is part-way through an upgrade, so it is
the riskiest to roll out and the one that stops `main` from being cut.
Keeping wire changes few and last leaves most of a cycle cuttable.

Guidance:

- The milestones follow the phases: m1 additive, m2 API breaks, m3 wire
  changes, with today's later-work milestones moving down one. An API break
  is what a published consumer adapts to when it rebuilds: the public Rust,
  JS and bindings API, CLI flags, config keys and environment variables. A
  wire change is what a deployed peer adapts to without rebuilding: a frozen
  wire version. Decide where the hang catalog JSON and stored or exported
  container formats fall; they read like wire, since a deployed peer parses
  them. Urgent work of any kind still joins m0.
- The cut's shift performs the re-split, so the
  [release cut](/quest/m3/release/cut.md) requires this quest and its
  recurrence is rewritten here: it sorts into three kinds, not two, and it
  recurs per cut rather than per major release.
- Decide what holds a phase's order. A break that lands on `main` during the
  additive phase turns the next cut into a breaking one. Ordering the
  milestones through `Required` keeps `quest unblocked` from offering the
  work early, and `just api-diff --deny` can refuse the PR.
- A cut in the additive phase has no break to audit: it needs the
  [cut gate](/quest/m2/cut-gate/README.md)'s checks but not the migration
  audit or the API, feature and Cargo feature reviews. Say which checks each
  kind of cut runs.
- `just version` and `version.json` enforce one shared minor advanced at
  every cut. An additive cut skips the bump and lets release-plz propose
  patches. Decide how the JS packages and the bindings take a patch at an
  additive cut.
- After any publish, `just test wire-compat --remeasure` still lands the
  refreshed planned breaks.

Public API: none. Wire: none. The published version numbers change shape:
fewer shared minors, more patches.

## Related

- [Release cut](/quest/m3/release/cut.md) - performs the shift this quest redefines
- [Cut readiness](/quest/m2/cut-gate/README.md) - the checks every kind of cut still runs
