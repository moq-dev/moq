# [M] RTMP sessions follow one strict path

## Goal

An RTMP connection, accepted or dialed, moves through one linear path
(handshake, connect, one stream, one publish or play) and refuses anything
off it. A peer can no longer hold a listener's pending slots with idle
sockets, grow per-session state by repeating requests, replace a live
broadcast through a publish on a stream it never created, or freeze a
broadcast with a timestamp rewind; a stalled server no longer hangs the
client forever.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670, all source-inferred
and inherited. The chunk layer is well bounded after a run of fixes
(f7433105d9, 4040d338ef, 0bd28eb165, 529d2101d6, 16875d620b, b0eca5022f);
the permissive vendored `ServerSession` above it is not.

- **Idle sockets lock out encoders:** `rs/moq-rtmp/src/server.rs:64` and
  `:348` stop accepting while 128 handshakes are pending, so an attacker
  re-opening 128 silent connections every 15 s blocks every encoder. Keep
  accepting, evict the oldest pending handshake, and give C0/C1 a short
  deadline of its own. This replaces a constant whose only effect is a
  refusal.
- **Requests held for the session's life:**
  `rml/sessions/server/mod.rs:534`, `:926`, and `:968` add to
  `outstanding_requests` (and `active_streams` on `createStream`); the pumps
  at `server.rs:1265` ignore those events and never remove them, and nothing
  is written back, so repeating `connect` with a 64 KB `app` grows state 1:1
  with no backpressure. Refuse a second `connect` as a protocol error and
  answer requests the gateway will not act on with `reject_request`.
- **Publish on a never-created stream** (`rml/sessions/server/mod.rs:846`):
  `Publish::accept` replaces the live broadcast under a fresh epoch before
  `accept_publish_request` fails, and the `Publisher` drops without
  `abort`, so subscribers see `Error::Dropped`. Validate the stream id in
  the session layer and reply `_error`.
- **Timestamp rewind freezes instead of ending:** `server.rs:1247` and
  `dial.rs:411` log a warning per frame on `TimestampRewind` and keep the
  connection, while `doc/bin/rtmp.md:39` says the push ends. Treat a rewind
  as fatal and abort the push, so the encoder's reconnect publishes a new
  epoch; per-tag demux faults stay non-fatal.
- **Client deadlines:** `dial.rs` has no deadline on connect/handshake
  (about lines 109-160) or `await_event` (440-478), and no media idle bound
  in `pull_media` (371-395). Reuse the server's request and idle budgets.

A strict linear state machine with typed command decoding removes the first
three by construction and shrinks the vendored surface; prefer it over three
point fixes. Note that every handshake runs inside `Server::accept`'s
`FuturesUnordered`, so a future rml panic would take down the listener;
none is reachable today.

Public API: none. Wire: none (refusals reply `_error`).

## Related

- [RTMP client errors](/quest/m2/rtmp-client-error.md) - `_error` reporting on the client
