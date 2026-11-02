# [M] Rust byte streams

## Goal

A Rust application opens a duplex byte stream on a
[socket](/quest/m2/socket/rust.md) and gets an `AsyncRead + AsyncWrite` that
carries TCP semantics over one bi track: ordered,
lossless, FIN as half-close, reset with a code. A socket's listener accepts the streams
its dialers open. `moq` gains TCP forwarding on both ends so an
unmodified client and server talk through a relay.

## Plan

See the [epic](/quest/m2/stream/README.md) for the mapping: a socket per
forwarded service, a bi track per connection with one group each way, a frame
per TCP read, no fill handler.

- Build on `moq_net::socket`: the path layout and connection naming are the
  socket's (`open_bi` and `accept_bi`), so this quest adds only the
  `AsyncRead`/`AsyncWrite` adapter and the TCP mapping. Place the adapter
  beside the socket module unless it pulls in a dependency moq-net avoids;
  report the choice.
- Each stream carries a subscription priority the caller sets; nothing is
  hard-coded below or above media.
- `moq` CLI: one verb forwards a local TCP listener to a remote stream and one
  serves a stream to a local TCP address; confirm the shape against `--help`
  and update `doc/bin/cli.md`.
- Tests on controlled time, no sleeps: a partial frame reaching the socket
  before the frame ends, round-trip bytes, half-close each way,
  reset each way, a reader ending `Evicted` surfaced as a reset, and a relay
  restart ending the stream with a typed error.

## Required

- [Rust sockets](/quest/m2/socket/rust.md) - the socket each stream is a bi track on
- [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md) - lossless delivery to a reader that keeps up
