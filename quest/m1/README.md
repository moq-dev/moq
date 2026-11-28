# m1: additive work for this cycle

## Goal

Until the `0.22.0` cut, only in-flight additive work that open PRs claimed
before 2026-11-28: each lands with its PR, and what a PR leaves moves to m3
once its claim ends. Non-breaking work the cut or lite-07 finalization
requires joins m2, and other additive work joins m3. The cut does not wait on
m1.

## Plan

Decided 2026-11-28 (maintainer, /quest-run): ✅ until the `0.22.0` cut, m0
to m2 hold only what the cut or lite-07 finalization needs, plus
semver-breaking API or wire changes; every other quest moved to m3 and ships
later in a patch version (rejected: moving only m1; keeping all of m0 as an
ops bucket). The [questline](/quest/README.md) states the rule.

Every m1 quest no open PR claimed moved to m3. Each quest still here was
claimed by an open PR on 2026-11-28 (`quest unblocked --unclaimed` lists the
current claims).

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
