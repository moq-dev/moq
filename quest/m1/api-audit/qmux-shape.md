# [S] qmux shape

## Goal

qmux binds its framing once, at connect or accept, uses moq-tokio's names
for flow control in Rust and JS, has no order-dependent setters, and the JS
package speaks only the wire Rust speaks.

## Plan

Decided 2026-10-22 (API audit):

- `transport::Stream::new(io, version, max_record_size)`
  (`rs/qmux/src/transport.rs`) repeats what `Config` holds and breaks framing
  silently on a mismatch; both consumers build the config twice. Take
  `Stream::new(io)` and bind framing in `connect`/`accept`.
- Flow control has four vocabularies (qmux `max_data`/`max_stream_data`,
  `moq_quic::mux` `max_concurrent_bidi_streams`, moq-tokio
  `receive_window`/`stream_receive_window`, JS `maxData` plus a bigint
  `maxIdleTimeout`). Adopt moq-tokio's names in qmux and `@moq/qmux`;
  `max_datagram_frame_size` stops using 0 for off.
- `#[doc(hidden)]` `moq_quic::mux`, public only for qmux.
- `tcp::Config`/`uds::Config` setters must be called in order and
  `session_config` drops version and protocol; use
  `tcp::{connect, accept}(.., Config)` and the `with_session_config` name
  tls/ws use.
- Delete `qmux::ALPNS` (duplicates `Version::ALL`), `Config::negotiated`,
  `SendStream::reset_at`, and make `Session::send_group` private.
- `@moq/qmux` still speaks the legacy `webtransport` wire Rust removed in
  #6231, falls back to it on accept, and defaults `frame.encode`/`decode` to
  it. Delete it.

Public API: qmux, moq-quic, `@moq/qmux` breaking. Wire: JS drops the
removed legacy framing.
