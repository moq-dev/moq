# [S] WebRTC egress follows a replaced sibling broadcast

## Goal

A `moq-webrtc` egress rendition that reads a sibling broadcast (the catalog's `broadcast` field)
keeps playing when that sibling's publisher is replaced, even if the catalog itself doesn't change.

## Plan

Found while iterating [moq-dev/moq#9807](https://github.com/moq-dev/moq/pull/9807); the behavior
predates it. `rs/moq-webrtc/src/egress.rs` resolves a rendition's `Target` once and re-picks only on
catalog changes, so a sibling broadcast that goes away and comes back under a new publisher leaves
the rendition on the dead subscription. Re-resolve when the sibling's announcement changes, learning
it through MoQ (the origin's announcements) rather than a timer. Add a test beside the existing
"moved to a sibling broadcast" case.

Public API: none. Wire: none.
