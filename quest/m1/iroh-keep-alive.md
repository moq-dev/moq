# [XS] iroh honors quic.keep_alive

## Goal

The iroh backend sends keep-alives at `quic.keep_alive`, like the QUIC
backend, and the docs stop saying iroh has no knob.

## Plan

Set `keep_alive_interval` from `quic.keep_alive` in `rs/moq-tokio/src/iroh.rs`
(the locked iroh 1.2 already has
`QuicTransportConfigBuilder::keep_alive_interval`, so no bump), and fix the
docs that say iroh has no knob (`rs/moq-tokio/src/quic.rs`,
`rs/moq-tokio/src/iroh.rs`, `doc/bin/relay/config.md`). Split from
[io_uring handshake deadline](/quest/m1/listener-deadlines.md) on 2026-10-08.

Public API: none. Wire: none.
