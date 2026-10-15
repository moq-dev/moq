# [XS] A malformed object in a group FETCH ends its track

## Goal

On a TIMESCALE track, a subgroup object without a Timestamp in a standalone
group FETCH, which a relay sends to fill a cache miss (`run_group_fetch` in
`rs/moq-net/src/ietf/subscriber.rs`), ends the whole track with
`MalformedTrack`, as it already does in `recv_group` and a joining FETCH's
fill (`recv_fill`). A regression test pins it.

## Plan

Decided 2026-10-14 by the maintainer, from
[#4822](https://github.com/moq-dev/moq/pull/4822)'s untimed follow-ups: the
publisher broke the track however the object arrived, so failing only the
group is not enough. js/net already ends the track
([#4968](https://github.com/moq-dev/moq/pull/4968)).
