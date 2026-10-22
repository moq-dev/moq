# [M] SUBSCRIBE_TRACKS in js/net

## Goal

`@moq/net` answers a moq-transport SUBSCRIBE_TRACKS (draft-18 on) as Rust
does, instead of refusing it with NOT_SUPPORTED: a paused PUBLISH per listed
track for FORWARD=0, resumed and paused with REQUEST_UPDATE; PUBLISH_SKIPPED
past the subscription cap and for every track of a FORWARD=1 listing from
draft-19; NOT_SUPPORTED for FORWARD=1 on draft-18; PREFIX_OVERLAP for an
overlapping prefix; and draft-18's DUPLICATE_SUBSCRIPTION for a SUBSCRIBE to
a PUBLISHed track.

## Plan

Mirror `rs/moq-net/src/ietf/publisher/tracks.rs`, reading tracks through the
TRACKS listing that [TRACKS in js/net](/quest/m1/archive/tracks-js.md) adds.
Mirror its tests, and drop the JavaScript caveat from
`doc/concept/standard.md`.

## Required

- [TRACKS in js/net](/quest/m1/archive/tracks-js.md) - `@moq/net` lists a broadcast's tracks, which this listing reads
