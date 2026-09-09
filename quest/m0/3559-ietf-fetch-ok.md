# [S] A FETCH is answered with FETCH_OK

## Goal

A successful FETCH gets FETCH_OK (0x18) on every negotiated draft, never
REQUEST_OK (0x7), and its End Location names a range the subscriber will accept
instead of one that earns a PROTOCOL_VIOLATION. A joining subscriber that pairs
SUBSCRIBE with a joining FETCH, which is how MSF-01 clients retrieve a catalog,
completes the fetch and plays.

Boundaries: the fetch response stays empty. The relay opens the data stream,
writes the FETCH_HEADER and FINs it, exactly as today, because below draft-20
`subscribe_range` ignores the filter and serves `ServeRange::default()`, which
starts at the beginning of the latest group, so the group a joining fetch would
backfill is already arriving on the subscription. Standalone fetches, absolute
joining fetches, and a non-zero group offset stay refused. Serving a joining
fetch for real is not worth building: draft-20 removed the joining variant of
FETCH in favor of the fill fetch streams moq-net already serves. `js/net` throws
"FETCH messages are not supported" and gets no mirror.

## Plan

What the tree does today:

- `write_fetch_ok` (`rs/moq-net/src/ietf/publisher.rs:1251`) encodes
  `ietf::FetchOk` only on draft-14. The `Draft15 | Draft16` arm and the `_`
  default both encode `ietf::RequestOk`, so drafts 15 through 20 answer with the
  wrong message. moq-playa reports it as
  `expected FETCH_OK/REQUEST_ERROR on request stream, got REQUEST_OK`.
- The End Location is the literal `Location { group: 0, object: 0 }`.
- `run_fetch_stream` binds the joining request id to `_subscribe_id` and drops
  it, so nothing connects the fetch to the subscription it joins.

What the drafts say. Section 5.2, in the same words on draft-15 through
draft-20, requires "exactly one FETCH_OK or REQUEST_ERROR in response to a
FETCH". REQUEST_OK's own definition lists the requests it answers, PUBLISH,
REQUEST_UPDATE, TRACK_STATUS, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS and
PUBLISH_NAMESPACE, and FETCH is not among them on any draft. Section 10.13
requires End Location to be at least the corresponding FETCH's Start Location,
"otherwise the receiver MUST close the session with a PROTOCOL_VIOLATION", and
section 10.12.2.1 puts a relative joining fetch's Start at
`{Joining Location.Group - Joining Start, 0}`. With the group hardcoded to 0,
every track whose live edge is past group 0 earns that close, so fixing the
message type alone would trade one interop failure for another.

`ietf::FetchOk` already has the per-draft layout: Request ID and Group Order on
draft-14, Request ID alone on 15 and 16, neither from 17 on, with Track
Properties as the trailing field. It has round-trip tests per version. Only its
caller is wrong.

The work:

- `write_fetch_ok` encodes `ietf::FetchOk` on every version, with
  `request_id: Some(..)` through draft-16 and `None` from draft-17 on, which is
  what the type's own assertions require.
- Give the fetch the one fact it needs to name a legal range: the group
  `run_subscribe_stream` already snapshots from `live_edge`, recorded under its
  request id and dropped when the subscription ends. Every stream runs on its
  own `Publisher` clone, so a plain field would be copied per stream and the
  fetch would never see the subscribe's entry; the map has to sit behind a
  shared handle so all clones read one table. Keep the entry to the group, since
  serving the range is explicitly out of scope.
- End Location becomes the fetch's own Start, `{group, 0}` for the offset-0
  joining fetch we accept: the empty range, and the smallest answer that is not
  smaller than Start. `end_of_track` stays false.
- A joining fetch naming a request id with no subscription keeps the existing
  refusal, alongside the standalone, absolute joining and non-zero offset cases.
  The literal `500` stays for now; [IETF error codes](/quest/m0/ietf-error-codes.md)
  already lists `run_fetch_stream` as one of its sites and replaces it with the
  registered value.

Tests, per version, since the message type is version-dependent and no test
covers what the publisher actually writes:

- A relative joining FETCH with offset 0, arriving while its subscription is
  still active, yields a response whose decoded message type is FETCH_OK
  (0x18) rather than REQUEST_OK (0x7), with that draft's field layout. Assert
  on the decoded type, not on 0x7 being absent from the bytes: 0x7 is a legal
  group id, object id or length, so scanning for it would fail a correct
  response.
- On a track whose live edge is past group 0, the End Location is that group
  with object 0 rather than `{0, 0}`. This is the regression test for the
  session close.
- The refused forms still produce their error message and close the writer, and
  a joining FETCH arriving after its subscription ended finds no entry and is
  refused rather than reading a stale group.

## Closes

- [#3559](https://github.com/moq-dev/moq/issues/3559) - close this issue when the quest finishes

## Related

- [IETF error codes](/quest/m0/ietf-error-codes.md) - replaces the 500 the refusals still use
- [TRACK_STATUS refusal](/quest/m0/3492-ietf-track-status-refusal.md) - the same shape: answer the request properly instead of leaving the peer guessing
