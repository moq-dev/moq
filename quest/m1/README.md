# m1: additive work the cut needs

## Goal

Improvements, fixes, and checks that need no semver-breaking release but that
the `0.22.0` cut or lite-07 finalization requires. Breaking work for the same
cut lives in m2; other additive work lives in m3 and ships in a patch version.

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

Every unclaimed m1 quest moved, since the cut does not require m1. What
remains is in-flight work an open PR claims; it lands when its PR does, and a
PR that leaves work moves the remainder to m3 once its claim ends.

Decided 2026-11-05 (maintainer, milestone re-cut): m1 is the additive half
of what was "everything before the cut" (2026-10-20). A quest stays here when
it would need no breaking release if it landed right after the cut; the rest
moved to [m2](/quest/m2/README.md), and m1's earlier decision log moved with
them, since it is the cut's. The cut no longer waits on m1: what lands before
it ships with it, and the rest ships at a later cut or as a `release` patch.

Placements decided in the re-cut: read-backfill (a JS fix that matches Rust),
buffered-ascending (lite-07 already carries the order field), webrtc-vendor
(internal, though it requires the m2 rename), and the crates.io trusted
publisher and OIDC quests (setup after the cut's first publishes) are
additive. video-vaapi moved up from later work
because a downstream iGPU transcode trial needs it now.

Decided 2026-11-13 (maintainer, /quest-run): stream-credit stays in m1 and
no longer gates m2, since takeover, its only wire change, moved to m3.
Its YIELDED continuations leave lite-07 again in m2
([moq-dev/moq#8965](https://github.com/moq-dev/moq/pull/8965), 2026-11-16).

Decided 2026-11-12 (maintainer, /quest-audit before the cut):

- ✅ Additive and docs-only work that sat in m2 moved here, so the cut stops
  waiting on it: the time-addressing guide, the JS fills that carry a time,
  and outbound federation (rejected: leaving each in m2 beside the breaking
  work it follows). Each still lists the m2 quest it builds on under
  `Required`. Superseded 2026-11-13, below.
- ✅ The mesh libraries and byte streams moved here from m3 (rejected:
  staying first in m3). The 2026-11-03 reason to keep them out of m1 was that
  the cut would wait on them; since 2026-11-05 the cut waits on m2 alone, and
  a downstream application builds on them from `main` now. Superseded
  2026-11-13, below, and 2026-11-14 (maintainer): byte streams moved to Kixel,
  which builds its own on moq-net's public API, and sockets dropped their
  `tcp` protocol. Socket endpoints followed on 2026-11-16 (maintainer,
  /quest-plan of the pre-cut audit): [they leave moq](https://github.com/moq-dev/moq/pull/9029)
  for Kixel before the cut.
- ✅ Also moved here: the two reconnect fixes whose bugs came from PRs that
  are on `main` and not on `release`, so the cut would publish them; the Go
  module proxy, since the mirror repository nears 1 GB; and the ashpd bump.

Decided 2026-11-13 (maintainer, /quest-plan): no quest requires one in a later
milestone, so the 14 m1 quests that waited on m2 work moved to
[m3](/quest/m3/README.md): buffered-ascending, export-order,
receive-windows-evicted, route-trace-validation, routing-guide,
subscribe-fills-js, time-addressing-guide, and the cluster-routing and mesh
epics whole. ✅ m3 (rejected: m2, where the cut would wait on them; pulling
their 12 breaking m2 blockers into m1). m3 is not gated, and they rank right
after m2, which is when they can start.

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): tests
that guard what production traffic reaches join m1 ahead of the questline's
m4 default, so changes are proven before they ship: the
QUIC Interop Runner (`test/quic-interop`), sim rule checks,
relay parser fuzz targets (qmux, moq-auth), and from m4
the relay on the sim (landed),
mutation testing (#8712), and peer-reachable unsafe. OSS-Fuzz stays in m4 (maintainer, same round): it mostly waits on
google/oss-fuzz.
Left in m4 without a pick: Kani, Miri seeds and TSan, the JS corpus, and
bindings interop.

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
