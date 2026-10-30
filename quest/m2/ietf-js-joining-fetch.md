# [S] The JS publisher answers a joining FETCH after a quick finish

## Goal

On draft 18, the JS publisher answers a joining FETCH that arrives after
its subscription finished cleanly, from the subscription's saved Largest
group, as the Rust publisher will. The `tail` interop cells with a native JS
or browser publisher pass on a track that finishes right after
SUBSCRIBE_OK.

## Plan

Decided 2026-10-25 (maintainer): honoring Largest Object and serving the
head moved into JavaScript FETCH in m1, which landed; only this part stays
in m2. A FETCH that resolves before PUBLISH_DONE already finishes its head.

In the tail test the track is finished before SUBSCRIBE_OK goes out, so
PUBLISH_DONE follows at once and the FETCH arrives to no subscription. Keep a
finished subscription's join answerable under the bound
[Join after a quick finish](/quest/m2/ietf-join-after-finish.md) settles for
Rust, and mirror it on the joinable-SUBSCRIBE registry js-fetch adds.

Tests: a unit test for a FETCH that arrives after PUBLISH_DONE, the bound's
refusals mirrored from Rust, and the tail cells
`just test interop --tail --publishers js-native-node,js-native-bun --subscribers rust --versions ietf`
plus the browser publisher.

Public API: none. Wire: none; a late joining FETCH is answered where it was
refused.

## Required

- [Join after a quick finish](/quest/m2/ietf-join-after-finish.md) - settles the bound this reuses
