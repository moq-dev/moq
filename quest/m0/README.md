# m0: immediate priorities

## Goal

The operational work the `0.22.0` cut needs now: passing Nightly, Interop,
and Video runs, which the cut's release gate runs, the test flakes that turn
them red, and the rind 0.4 adapters. CI cost and `release`-line backports
that need not block the cut live in m3; the ci epic stays here only until its
child mbx-b2's claim ends, then moves to m3 whole.

## Plan

Decided 2026-11-28 (maintainer, /quest-run): ✅ until the `0.22.0` cut, m0
to m2 hold only what the cut or lite-07 finalization needs, plus
semver-breaking API or wire changes; every other quest moved to m3 and ships
later in a patch version (rejected: moving only m1; keeping all of m0 as an
ops bucket). The [questline](/quest/README.md) states the rule.

Here, js-listing-synced-order and release-attest-backport moved. The
interop-green and nightly epics and rind-adapters stay as cut blockers, and
the test-flakes-5 epic stays because its interop finish-lane flakes fail the
Interop run the cut's gate runs. The ci epic (its child mbx-b2 is claimed) and
repeatable-instruction-counts stay only until their claims end.

The release API gates (#3829..#3878) and the release that followed them are
done.

Per-session request caps landed in #4820, idle fronts in #5054, and the
lost demand-poll wake in #5091.

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.
