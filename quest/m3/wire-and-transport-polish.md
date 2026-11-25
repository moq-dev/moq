# [S] Polish wire, transport, qmux config, draft, and HLS escaping edge cases

## Goal

`FetchServeRun` closes streams cleanly with FIN, `draft-lcurley-moq-ietf.md`
matches the implemented compression and property limits, `moq-tokio` exposes
stream TLS certificates and rejects WebTransport requests gracefully, `qmux`
validates rate caps consistently, and `moq-hls` escapes DASH `$` templates and
sanitizes playlist query strings.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ collect these real low-severity Rust wire, transport,
draft, and HLS bugs into a compact `m3` area quest (rejected: dropping low-
severity bugs).

1. **`rs/moq-net/src/lite/subscriber.rs:14052-14066`**: call
   `stream.writer.finish()` when `FetchServeRun` succeeds instead of dropping
   the writer and emitting `RESET_STREAM(CANCELLED)`.
2. **`rs/moq-tokio/src/server.rs:422,505-511` &
   `rs/moq-tokio/src/quic/web/server.rs:48-55`**: fall back to
   `self.tls_stream` in `Server::certificates()` when QUIC TLS is absent, and
   wait gracefully on stream finish in `Request::reject` instead of abruptly
   dropping the connection.
3. **`rs/qmux/src/config.rs:147-151` & `rs/qmux/src/ws.rs:222,396`**: reject
   invalid rate caps consistently with `set_limits`, and fix stale
   `with_session_config` doc comments in `ws.rs`.
4. **`rs/moq-hls/src/export/mpd.rs:153-158` &
   `rs/moq-hls/src/export/playlist.rs:66-118`**: escape `$` as `$$` in DASH
   MPD query strings (per ISO/IEC 23009-1 template substitution) and sanitize
   newlines/control characters in `query` in `render_media`.

Public API: none. Wire: none (clean FIN instead of reset on fetch completion;
draft text synced to implementation). Add unit tests for each fix and run
`just drafts check`.
