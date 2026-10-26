# [M] Rust byte streams

## Goal

A Rust application opens a duplex byte stream to a peer by path and gets an
`AsyncRead + AsyncWrite` that carries TCP semantics over a track pair: ordered,
lossless, FIN as half-close, reset with a code. A listener accepts the streams
dialed under its prefix. `moq` gains TCP forwarding on both ends so an
unmodified client and server talk through a relay.

## Plan

See the [epic](/quest/m2/stream/README.md) for the mapping: one track per
direction, a group per connection, a frame per socket read, no fill handler.

- Place it where the existing patterns put a reusable wire-level primitive
  (a module in `moq-net` or its own crate); report the choice.
- The dialer publishes its outbound track under its own path and the listener
  discovers it by announcement, then answers on its own path. Settle the exact
  path layout and how the two sides name a connection before coding, and record
  it in this quest.
- Each stream carries a subscription priority the caller sets; nothing is
  hard-coded below or above media.
- `moq` CLI: one verb forwards a local TCP listener to a remote stream and one
  serves a stream to a local TCP address; confirm the shape against `--help`
  and update `doc/bin/cli.md`.
- Tests on controlled time, no sleeps: round-trip bytes, half-close each way,
  reset each way, a reader ending `Evicted` surfaced as a reset, and a relay
  restart ending the stream with a typed error.

## Required

- [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md) - lossless delivery to a reader that keeps up
