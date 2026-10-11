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

- Decided 2026-10-11: `main` likely already handles this (the report ran on
  the release), so reproduce before fixing. Run the drill on the
  `moq-relay-v0.17.2` tree too, to confirm it catches the reported toggling;
  a passing `main` drill counts as evidence only after that positive control
  fails on the release.
- Suspected mechanism, unconfirmed: path hunting in today's lite-06 path
  vector. A peer's re-announce of a withdrawn prefix clears its withdrawal
  (`reannounced` in `rs/moq-net/src/model/origin.rs`) and revives stale
  alternate routes through it, so each node's late withdrawal toggles the
  path again. A two-relay cluster has no alternates, which fits the report.
  #4642's 300 ms hold does not cover it: it delays only updates to a
  prefix already delivered, and each toggle is a fresh retract or announce.
- If `main` is clean, land the drill and close #4349 asking the reporter to
  re-run once the next release reaches cdn.moq.pro. If `main` still flaps,
  fix it at the source in the path vector when the fix is small; otherwise
  land the drill as an acceptance test for
  [Routes and announces](/quest/m1/cluster-routing/routes.md), keep #4349
  open, and move this `Closes` entry there.
- Not planned here: the reporter's self-hosted two-relay rejoin stall
  (`subscribe canceled (idle)`, then `old`, then no state). On 0.17.2 a
  rejoin could read a stale cached group as the live edge, which #4741 and
  #4914 changed on `main`; revisit it if their re-run on a release
  carrying those still stalls.
- Harness for reference: https://github.com/fperex/moq-burst-repro (published
  crates only); its traces hold per-session announcement sequences.

## Closes

- [#4349](https://github.com/moq-dev/moq/issues/4349) - close this issue when the quest finishes

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the lite-07 redesign that ends path hunting by construction
- [Dropped sources](/quest/m1/dropped-sources.md) - the `internal error` replies seen during the same churn
