# [XS] A new subscriber's datagrams start within its max delay

## Goal

A new Rust moq-net datagram subscriber starts at the first buffered datagram
within the subscription's `max_delay` of the highest buffered timestamp
(converted from the track timescale). Since #4982 it may otherwise get the
whole 64-datagram send buffer (`MAX_DATAGRAMS` in
`rs/moq-net/src/model/track.rs`), which on a sparse track can be minutes old.
Untimed tracks keep today's behavior.

`@moq/net` already replays nothing to a late subscriber: its 64-entry ring is
per subscriber and fills only after the sink attaches (`js/net/src/track.ts`),
which is within the bound. Keep that, and pin it with a test.

## Plan

Decided by the maintainer (2026-10-08): "fine to serve old datagrams from
the last ~50ms", so the bound is the subscription's own `max_delay` measured
against the newest datagram's timestamp, not wall time. Only where a new
datagram cursor starts changes; delivery after that is unchanged.

Test with mocked time in Rust: a sparse timed track with old and recent
datagrams buffered, including one inserted out of order, gives a new
subscriber only those within its `max_delay` of the highest timestamp, and an
untimed track still gives the whole buffer. In JS, a late subscriber gets no
datagram written before it attached, timed or untimed.

Public API: none. Wire: none.
