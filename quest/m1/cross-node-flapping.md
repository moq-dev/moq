# [M] Reproduce cross-node announce flapping

## Goal

A time-mocked drill with three or more clustered relays, extending
`rs/moq-net/tests/mesh_withdraw.rs` (which already checks one retraction on a
34-node partial mesh) or added beside it: a publisher on one node cleanly closes a broadcast, and a client
watching its prefix on another node sees one retraction, with no
re-announce, for that path. The drill runs against `main` and lands as a
regression test.

Reported in [#4349](https://github.com/moq-dev/moq/issues/4349) against the
release on cdn.moq.pro (2026-10-11): after a clean close, the subscriber's
announce stream saw the path Announced/Retracted about 1140 times over about
2 s, and subscribes meanwhile answered `internal error` or `unroutable`.
It never happened on one node or on a self-hosted two-relay 0.17.2 cluster.
Loss and FETCH were clean in every run (about 31k FETCHes, all ok).

## Plan

- Updated 2026-10-13: #5375's two-second same-publisher retraction quarantine
  and #5391's repeated-retraction queue dedup landed. #5375 reports 1,042
  revived broadcasts reduced to 15 over 200 randomized meshes; the #5376
  release backport reports 457 reduced to 2. These are mitigation results,
  not proof that main is clean. Existing mesh_withdraw tests include the
  eight-node regression. Preserve those and reduce residual failures to
  deterministic public fixtures, with pre-fix/release positive controls.
  Git backports establish release inclusion, not a verified deployment build.
- The stale-revival mechanism investigated in #5375: path hunting in today's lite-06 path
  vector. A peer's re-announce of a withdrawn prefix clears its withdrawal
  (`reannounced` in `rs/moq-net/src/model/origin.rs`) and revives stale
  alternate routes through it, so each node's late withdrawal toggles the
  path again. A two-relay cluster has no alternates, which fits the report.
  #4642's 300 ms hold did not cover fresh retract/announce toggles. The newer
  quarantine holds strictly worse stale alternatives, leaving equal/better
  stale ranks. Do not lengthen the hold to declare the issue solved.
- Land the incident-sensitive baseline and residual fixtures before the route
  rewrite, without requiring a redesign here. Routes and announces owns
  closing #4349, so this quest can complete with the counterexample harness
  rather than depending on the implementation it gates. A passing single topology is not enough to close
  the residual concern. Fix a small independent root cause only if one is found.
- Not planned here: the reporter's self-hosted two-relay rejoin stall
  (`subscribe canceled (idle)`, then `old`, then no state). On 0.17.2 a
  rejoin could read a stale cached group as the live edge, which #4741 and
  #4914 changed on `main`; revisit it if their re-run on a release
  carrying those still stalls.
- Harness for reference: https://github.com/fperex/moq-burst-repro (published
  crates only); its traces hold per-session announcement sequences.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the lite-07 redesign that must pass the residual counterexamples
- [Dropped sources](/quest/m1/dropped-sources.md) - the `internal error` replies seen during the same churn
