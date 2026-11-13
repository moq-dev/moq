# m1: additive work for this cycle

## Goal

Improvements, fixes, and checks that need no semver-breaking release: each
lands on `main` and ships with the next cut, or as a `release` patch when it
is backported. Breaking work for the same cut lives in m2.

## Plan

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

Decided 2026-11-12 (maintainer, /quest-audit before the cut):

- ✅ Additive and docs-only work that sat in m2 moved here, so the cut stops
  waiting on it: the time-addressing guide, the JS fills that carry a time,
  and outbound federation (rejected: leaving each in m2 beside the breaking
  work it follows). Each still lists the m2 quest it builds on under
  `Required`. The handoff-revival fix stays in the routes epic, since its PR
  is open.
- ✅ The mesh libraries and byte streams moved here from m3 (rejected:
  staying first in m3). The 2026-11-03 reason to keep them out of m1 was that
  the cut would wait on them; since 2026-11-05 the cut waits on m2 alone, and
  a downstream application builds on them from `main` now.
- ✅ Also moved here: the two reconnect fixes whose bugs came from PRs that
  are on `main` and not on `release`, so the cut would publish them; the Go
  module proxy, since the mirror repository nears 1 GB; and the ashpd bump.

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
