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
[Cross-relay FETCH over moq-transport](/quest/m1/ietf-peer-fetch-old.md)
fixes the same FETCH path refusing held groups as `old`.

Serving downstream lifts the one-group refusal ("FETCH spanning several
groups not supported") in `run_fetch_stream`
(`rs/moq-net/src/ietf/publisher.rs`) for every draft, including draft-20's
`FetchType::Filtered` once [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md)
serves it.

## Required

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - the range requests this answers
- [Draft-20 FETCH](/quest/m1/ietf-fetch-location.md) - serves draft-20 FETCH through the same group-span path this widens (#4971)

## Related

- [Pipelined first FETCH](/quest/m1/pipeline-requests/fetch.md) - sends TRACK_STATUS alongside the FETCH in adjacent code (`ietf/subscriber.rs`, `model/origin.rs`); decided 2026-10-08 it lands first, since it is unblocked and this is not, and this rebases onto it
