# [S] Draft-20 FETCH

## Goal

On drafts 20 and later, a FETCH whose LOCATION_FILTER stays within one group
is answered the way drafts 14 to 19 answer a standalone FETCH: from cache, with
a miss fetched upstream, and an upstream refusal passed through. A range
touching several groups is refused `NOT_SUPPORTED`, as on older drafts.

## Plan

Today both sides refuse FETCH on draft-20+ `NOT_SUPPORTED`: the publisher in
`run_fetch_stream`, and the relay's upstream group fill in `run_group_fetch`,
because the codec still carries the removed Fetch Type field.
[Legal IETF input](/quest/m0/ietf-legal-input.md) fixes the codec. Then drop
both refusals, and flip `fetch_moq_transport_20` in `rs/moq-tokio/tests` back
to a served fetch.

Update the draft-20 note in `doc/concept/standard.md`.

## Required

- [Legal IETF input](/quest/m0/ietf-legal-input.md) - decodes the draft-20 FETCH this serves
