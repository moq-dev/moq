# [S] 0-RTT streams adopt a raised stream window

## Goal

When a server raised its stream receive window between sessions, a client's
0-RTT streams adopt the new window once the server's transport parameters
arrive, and run to completion. Today they keep the remembered smaller window,
and the server, believing the client already has the larger one, never sends
MAX_STREAM_DATA, so bidi and uni streams stall forever.

`zero_rtt_bidi_window_raise` and `zero_rtt_uni_window_raise` in
`rs/moq-quic/src/tests/characterize/limits.rs` must pass with their
`#[ignore]` removed.

## Plan

Decided 2026-10-16 (maintainer): `StreamsState::set_params` in
`rs/moq-quic/src/connection/streams/state.rs` raises the send window of the
client's own 0-RTT streams to `initial_max_stream_data_bidi_remote` (bidi) or
`initial_max_stream_data_uni` (uni), and wakes any writer the new credit
unblocks. RFC 9000 7.4.1 forbids a server that accepts 0-RTT from lowering
these, so the window only grows.

The same PR deletes the existing loop over peer-initiated bidi streams in
`set_params`, which is unreachable through the public API.
