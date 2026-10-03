# [S] Draft-20 FETCH

## Goal

On drafts 20 and later, a FETCH whose LOCATION_FILTER stays within one group
is answered the way drafts 14 to 19 answer a standalone FETCH: from cache, with
a miss fetched upstream, and an upstream refusal passed through. A range
touching several groups is refused `NOT_SUPPORTED`, as on older drafts.

## Plan

Today both sides refuse FETCH on draft-20+ `NOT_SUPPORTED`: the publisher in
`run_fetch_stream`, and the relay's upstream group fill in `run_group_fetch`.
The codec already decodes and encodes the draft-20 layout as
`FetchType::Filtered`, so drop both refusals, and flip
`fetch_moq_transport_20` in `rs/moq-tokio/tests` back to a served fetch. A
FETCH carrying Range Filters stays refused.

Update the draft-20 note in `doc/concept/standard.md`.

A route change exercises this end to end: `rs/moq-net/tests/route_change.rs`
`route_flaps` hangs on moq-transport 22, because the relay the track falls back
to fills its group's missing frames with a FETCH upstream, which stays pending
instead of being served or refused. Add `flaps_ietf_22` to `route_flap_tests!`
once it passes.
