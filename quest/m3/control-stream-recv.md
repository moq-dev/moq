# [M] Every control stream reads a typed Recv outcome

## Goal

The announce, probe, tracks, auth, and publisher control-stream readers in
moq-lite tell a reset, a clean end, and malformed bytes apart through
`coding::Reader::poll_recv`'s `Recv`, the way the subscriber's response reads
already do. `lite::error::control` and `auth::protocol_error`, which
classify an `Error` by matching its variants, are deleted (AGENTS.md: never
match on errors).

## Plan

The first `Recv` readers landed with
[moq-dev/moq#8931](https://github.com/moq-dev/moq/pull/8931): `Recovery`, the
live subscription's response read, and the drain after FINISH. Those three
reads in `rs/moq-net/src/lite/subscriber.rs` and `subscriber/fills.rs` are
the pattern: the reader classifies where it knows (a transport reset or a FIN
inside a message is `Reset`, a decode failure is `Malformed`), and the caller
branches on the outcome.

Callers left, from `rg 'error::control|protocol_error' rs/moq-net/src`:

- `lite::error::control`: the publisher's control stream
  (`lite/publisher.rs`), the subscriber's other control reads
  (`lite/subscriber.rs`, `subscriber/route_stream.rs`).
- `auth::protocol_error`: the auth token presenter (`lite/session.rs`), the
  publisher's auth serve (`lite/publisher.rs`), and `lite::error::control`
  itself.
- `DatagramServe` (`lite/subscriber.rs:2960, 3009-3012, 3179-3185`, added
  2026-11-21 from the pre-cut audit): currently logs and aborts the track on
  malformed `DATAGRAMS_INFO` or trailing messages on the datagram subscribe
  stream instead of closing the session with `SessionError::ProtocolViolation`
  via `Recv::Malformed`.

Each reader returns the outcome typed, and the session close on a malformed
message (`SessionError::ProtocolViolation`) moves to where `Malformed` is
read. Check the IETF side (`ietf/session.rs` `control_stream_error`) for the
same pattern and leave it out only if it cannot share the reader.

Public API: none. Wire: none. Cover each reader with a test of the three
outcomes, as `a_read_tells_a_cut_stream_from_malformed_bytes` does for the
reader.
