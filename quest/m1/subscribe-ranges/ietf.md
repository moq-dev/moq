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
A multi-group FETCH keeps its one queue entry and moves with priority updates
(decided 2026-10-13).

The same FETCH-end path sets End of Track only when the read hits FIN, so a
bounded FETCH that ends exactly at the track's last object reports
`end_of_track: false` (found while landing #4971; it predates it). Set it only
when the track has finished and the FETCH reaches its final object, never from
a live track's Largest Object, and test both cases.

Upstream range changes ride the subscription update that already carries the
aggregate priority (`write_update` in `ietf/subscriber.rs`). Narrowing is
allowed from draft-15 on. Widening is a bug today: on draft-20 and later, a
subscriber from a future group puts its absolute start in the SUBSCRIBE filter,
and a live subscriber merged in after it never gets the current group. Draft-20
lets an update lower the start with FILL_PARAMETERS for what it missed, which
opens a second fill stream on the subscription. Draft 14 forbids widening, and
15 to 19 have no fill. `rs/moq-net/tests/integration/subscribe_ranges.rs`
leaves draft-20+ out for that reason; add it back once the update carries the
widened filter.

moq-transport has no time filter, so a downstream `Time(pts)` end is
resolved against the relay's cache before it goes upstream as a group range,
or refused when the cache cannot resolve it.

## Related

- [Pipelined first FETCH](/quest/m2/pipeline-requests/fetch.md) - sends TRACK_STATUS alongside the FETCH in adjacent code (`ietf/subscriber.rs`, `model/origin.rs`); an independent m2 optimization that adapts to these ranges and does not gate them
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - handles native timeout gaps; do not mistake them for never-existing objects or media-age limits
