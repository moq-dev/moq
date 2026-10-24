# [M] A moq-transport mid-group subscription to a compressed track widens to frame 0

## Goal

An application range starting mid-group on a compressed track, requested over
moq-transport draft-17+ with the Flate extension negotiated, reads the frames
it asked for instead of failing with `Lagged` against a publisher that serves
exactly the requested Object range. The requesting IETF subscriber widens the
SUBSCRIBE start to Object 0 when it lacks the group's prefix, as lite-07 does
since [moq-dev/moq#6242](https://github.com/moq-dev/moq/pull/6242). The server
never widens and keeps serving exactly what is asked.

## Plan

The rule is the [epic](/quest/m1/compress/README.md)'s Ranges rule, decided
with the maintainer on 2026-10-20: the subscriber widens, never the server;
one that holds frames 0..N-1 still asks from N; the reader still starts at the
frame it asked for. Lite-07 applies it through `track::Producer::widen_compressed`
(`rs/moq-net/src/model/track.rs`), which a warm IETF copy can reuse.

Split from #6242 because a two-hop matrix (lite-07 and moq-transport-17, 18
and 22 on either hop) found three gaps under the IETF requester that are not
about compression:

- A cold SUBSCRIBE (`Target::Request` in `rs/moq-net/src/ietf/subscriber.rs`)
  learns the track is compressed only from SUBSCRIBE_OK. Widening every cold
  mid-group start on a Flate session would make plain tracks pay for the
  prefix too. Decide between learning the properties first (TRACK_STATUS where
  `TrackStatusOk::describes_track` holds) and widening cold starts only on
  Flate sessions.
- Readers get IETF-received groups whole: `track::Subscriber::deliver` skips
  the frame window when `group.relayed()`, since Object IDs may not be frame
  ordinals. An application on an IETF hop therefore already reads frames below
  its start on plain tracks, and widening would hand it more. Filter by
  ordinal when the copy's Object IDs are dense, or by Object ID.
- REQUEST_UPDATE carries no range, so a live range change never reaches
  upstream; [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) owns
  that.

In the matrix, our own IETF publisher served the whole group for a mid-group
Absolute start, so the `Lagged` failure needs a publisher that honors the
exact start; test against the publisher model directly.

`js/net` needs no mirror: its subscriptions carry group ranges only
(`Subscription.groups`), so a JS SUBSCRIBE always starts at frame 0, and a
compressed group that does not is refused with `TooFarBehind`.

Regression: extend
`application_ranges_keep_their_filter_after_requesting_the_compressed_prefix`
(`rs/moq-net/tests/integration/compress.rs`) over moq-transport-17, 18 and 22
hops.

Public API: none. Wire: none.

## Related

- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - carries a live range change upstream on REQUEST_UPDATE
- [Bounded tails](/quest/m1/subscribe-ranges/bounded-tail.md) - a copy cut at a request's end must accept its tail before a warm prefix can be extended
