# [M] Expose QUIC flow-control windows on quic::Client and quic::Server

## Goal

A Rust consumer of `moq-tokio` sets the QUIC flow-control windows through the
public `quic::Client` / `quic::Server` config, without patching the crate, and
every backend either applies the value or refuses it. The same fields reach
`moq-relay` TOML, CLI flags, and environment variables the way the existing
knobs do.

Boundaries: no raw `quinn::TransportConfig` / quiche `Settings` escape hatch,
since that leaks a third-party type into the signature. Congestion control
stays the `CongestionControl { Loss, Delay }` family; no algorithm-level
names. No moq-ffi, libmoq, or js/net surface in this quest.

## Plan

Everything else #816 asks for already exists on `quic::Client` and
`quic::Server` (`rs/moq-tokio/src/quic.rs`): `idle_timeout`, `keep_alive`,
`max_streams`, `gso`, `mtu_discovery`, `congestion_control`, `qlog`, each
wired to quinn, quiche, noq, and iroh, with clap long names, env vars, and
relay TOML. Flow-control windows are the one knob nobody can set.

Add three `Option<u64>` byte fields to `quic::Client`, `quic::Server`, and
`quic::Resolved`, `None` meaning the backend default:

- `receive_window`: the connection-wide receive window.
- `stream_receive_window`: the per-stream receive window.
- `send_window`: the cap on unacknowledged outgoing data.

Follow the existing field conventions: `--client-quic-receive-window` /
`--server-quic-receive-window` long names, `MOQ_CLIENT_QUIC_RECEIVE_WINDOW`
env vars, serde for `[client.quic]` / `[server.quic]`, and validation next to
`validate_idle_timeout` (a window must fit a QUIC varint and must not be zero).

Backend wiring, in each backend's private apply function:

- quinn (and noq, which shares the shape): `TransportConfig::receive_window`,
  `stream_receive_window`, `send_window`.
- quiche: `initial_max_data` for the connection window and
  `initial_max_stream_data_bidi_local`, `_bidi_remote`, and `_uni` all from
  the stream window. quiche has no local send cap, so a `send_window` on the
  quiche backend is refused at resolve time with a config error, never
  silently dropped.
- iroh: apply through its transport config where quinn's fields are
  reachable; refuse anything it cannot apply, the way `gso = false` is refused
  today.

Docs: `doc/bin/relay/config.md` documents the new fields and, while there,
the existing `[server.quic]` / `[client.quic]` fields it omits today
(`max_streams`, `idle_timeout`, `keep_alive`, `gso`, `mtu_discovery`, `qlog`,
`preferred_v4`, `preferred_v6`, `quic_lb_id`, `quic_lb_nonce`), since
`congestion_control` is the only one on the page. `doc/lib/rs/env/tokio.md`
shows the library form.

Tests: a resolve test per field; a `moq-relay` config-merge test guarding the
windows against the CLI re-parse clobbering TOML, like the existing
`congestion_control` guard; a refusal test for `send_window` on quiche; and one
backend test per backend where the built transport config is inspectable.

Branch from `dev`: the crate is `moq-tokio` there. The change is additive.

## Closes

- [#816](https://github.com/moq-dev/moq/issues/816) - close this issue when the quest finishes
