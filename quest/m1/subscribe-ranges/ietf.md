# [M] moq-transport ranges

## Goal

A relay fills past-range misses from a moq-transport upstream with one
standalone FETCH per run of locally missing groups, never including the
upstream's live group, so a sparse track costs a handful of requests. It
serves a downstream IETF FETCH from the model's ranges without blocking on the
live group.

## Plan

This absorbs the relay half of fetch-span (#4558) and builds on fetch-fill's
checks (#4544). Receiving a multi-group FETCH stream means several groups on
one stream. Absent sequences between the FETCH's groups become drops, and a
group already cached is discarded as a duplicate. Cap a downstream FETCH at
the Largest Object, as the moq-transport drafts require. A relay with only
fetch demand learns the upstream's Largest from TRACK_STATUS without a
SUBSCRIBE, since #4974.

Serving downstream lifts the one-group refusal ("FETCH spanning several
groups not supported") in `run_fetch_stream`
(`rs/moq-net/src/ietf/publisher.rs`) for every draft, including draft-20's
`FetchType::Filtered`. Capping at the Largest Object also lifts the draft-20
refusal of a filter bounded by it ("FETCH relative to Largest Object not
supported"): no filter, a relative start, or an absolute start with no end.

The same FETCH-end path sets End of Track only when the read hits FIN, so a
bounded FETCH that ends exactly at the track's last object reports
`end_of_track: false` (found while landing #4971; it predates it). Set it only
when the track has finished and the FETCH reaches its final object, never from
a live track's Largest Object, and test both cases.

## Required

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - the range requests this answers

## Related

- [Pipelined first FETCH](/quest/m1/pipeline-requests/fetch.md) - sends TRACK_STATUS alongside the FETCH in adjacent code (`ietf/subscriber.rs`, `model/origin.rs`); decided 2026-10-08 it lands first, since it is unblocked and this is not, and this rebases onto it
- [FETCH max-delay](/quest/m1/fetch-max-delay.md) - preserve local age enforcement and the omitted-budget default when adapting IETF FETCH to ranges
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - handles native timeout gaps; do not mistake them for never-existing objects or media-age limits
