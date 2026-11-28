# m0: immediate priorities

## Goal

The operational work the `0.22.0` cut needs now: passing Nightly, Interop,
and Video runs, which the cut's release gate runs, the test flakes that turn
them red, and the rind 0.4 adapters. CI cost and `release`-line backports
that need not block the cut live in m3.

## Plan

Decided 2026-11-28 (maintainer, /quest-run): ✅ m0 to m2 focus on the lite-07
freeze and the `0.22.0` cut. A quest stays in m0 to m2 only when the
[cut](/quest/m3/release/cut.md)'s `Required` (all of m2, rind-adapters, and the
interop-green and nightly epics) or [lite-07 finalization](/quest/m2/lite07-finalize.md)
needs it, directly or transitively, or when it makes a semver-breaking API or
wire change. The rest moved to [m3](/quest/m3/README.md) and ships later in a
patch version; additive API (a new field on a `#[non_exhaustive]` struct, a new
flag or function) moves too. (Rejected: moving only m1; keeping all of m0 as an
ops bucket.) A quest an open PR claims stays where it is, since `quest mv`
refuses to move a claimed quest; once its claim ends, move what remains with
`quest mv`.

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
