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

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): tests
that guard what production traffic reaches join m1 ahead of the questline's
m4 default, so changes are proven before they ship: the
[QUIC Interop Runner](/quest/m1/quic-interop.md),
[sim rule checks](/quest/m1/quic-sim-rules.md),
[relay parser fuzz targets](/quest/m1/fuzz-relay-parsers.md), and from m4
[the relay on the sim](/quest/m1/quic-sim.md),
mutation testing (#8712), and
[peer-reachable unsafe](/quest/m1/wire-fuzz-coverage.md). OSS-Fuzz stays in m4 (maintainer, same round): it mostly waits on
google/oss-fuzz.
Left in m4 without a pick: Kani, Miri seeds and TSan, the JS corpus, and
bindings interop.

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
