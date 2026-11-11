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

Held here by open claims: a quest's branch is its path, so a claimed quest
cannot move until its PR merges, and an epic moves with its children. These
breaking epics and quests stay in m1 for now and are listed in m2's
`Required`, so the cut still waits on them: drop-set and
stream-credit. The [re-cut follow-up](/quest/m2/recut-held.md)
moves what remains of them once their claims merge.

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
