# [S] moq-ffi can disable or delay the WebSocket fallback

## Goal

A moq-ffi caller, and every wrapper over it (py, swift, kt, go, dart), can
disable the WebSocket fallback or change the head start QUIC gets before it,
the way the CLI (`--connect-websocket-enabled`, `--connect-websocket-delay`)
and libmoq's `moq_client_config` already can. Today a wrapper user on a
WebTransport-only relay has no way to stop the fallback from racing at all.
Additive, on main after the dev merge lands `rs/moq-tokio` there.

## Plan

`MoqClient` (`rs/moq-ffi/src/session.rs:310`) holds a
`moq_tokio::connect::Config` (line 381) and exposes it through setters that
each lock the task state and write one field: `set_tls_disable_verify` at
line 389 writes `config.tls.insecure`, `set_reconnect` at 469 writes
`config.once`, `set_backoff` at 476 unpacks a `MoqBackoff` record of `_ms`
fields into `config.backoff`. The fallback knobs live at
`config.websocket`: `websocket::Config` (`rs/moq-tokio/src/websocket.rs:114`)
with `enabled: Option<bool>` (131, `None` means on) and `delay: CliDuration`
(141, default 200 ms). libmoq already maps both: `moq_client_config`
(`rs/libmoq/src/api.rs:843`) carries `websocket_enabled` and
`websocket_delay_ms` with `has_` flags (874 to 879), applied in
`rs/libmoq/src/client.rs:64` to `:69`, and defaults exported at
`api.rs:977`.

Shape, two options:

- Two setters beside the tls ones, `set_websocket_enabled(bool)` and
  `set_websocket_delay(delay_ms: u64)`, mirroring the libmoq fields and the
  `_ms` convention `MoqBackoff` uses. Recommended: smallest surface, and
  every wrapper already has a one-line pattern for a boolean setter.
- One `set_websocket(MoqWebsocket { enabled, delay_ms })` record like
  `set_backoff`. Only worth it if a third knob appears.

Cross-package sync from the root CLAUDE.md table, with the lines that mirror
`set_tls_disable_verify` today:

- `rs/libmoq`: already has both fields; nothing to add.
- `py/moq-rs/moq/client.py:80` (constructor kwargs, applied in `__aenter__`).
- `swift/Sources/Moq/Client.swift:15`.
- `kt/moq/src/jvmAndAndroidMain/kotlin/dev/moq/Moq.kt:109`.
- `go/wrapper/client.go:194` (the table names `go/wrapper/moq/*.go`; the
  wrapper lives at `go/wrapper/` in this tree). `go/ffi` regenerates.
- `dart/moq/lib/moq.dart:32`. `dart/moq_ffi` regenerates.
- `doc/lib/{py,swift,kt,go,dart}/index.md`: add the two options to each
  client example.

Tests: a moq-ffi unit test that each setter lands in `config.websocket`, and a
wrapper test where the tls setter is covered today
(`py/moq-rs/tests/test_server.py`). `just test smoke-full` for the bindings.

## Related

- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - the race that a disabled fallback sidesteps entirely
