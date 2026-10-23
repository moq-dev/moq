# [M] io_uring MTU matches tokio

## Goal

An io_uring relay connects over any path that carries 1200-byte QUIC
payloads, exactly as the tokio backend does, and `quic.mtu_discovery` is
honored instead of refused: off by default, discovering larger MTUs when on.

## Plan

`rs/moq-uring/src/quic/driver/mod.rs` pins the initial and minimum MTU to
`SEGMENT` (1350), disables discovery, and advertises a 1350
`max_udp_payload_size`; `rs/moq-relay/src/uring.rs` refuses
`quic.mtu_discovery`. Tokio uses moq-quic's 1200 floor and 1472
advertised, and maps the resolved `quic.mtu_discovery` (default false)
onto `mtu_discovery_config` (`rs/moq-tokio/src/quic/backend.rs`). A path below ~1378 bytes (IPv4) or ~1398
(IPv6) black-holes on the ring with no fallback.

The pin dates from the quiche-era driver and was never re-justified. Its GSO
rationale no longer holds: `flush_one` already sends with moq-quic's own
`transmit.segment_size`, and moq-quic sizes GSO batches itself (#6279). Only
`segments = capacity / SEGMENT` in `connection.rs` assumes the fixed size;
derive it from the current MTU instead.

Drop the overrides so the ring uses the same `TransportConfig` defaults as
tokio, add the resolved setting to `moq_uring::quic::Transport` so
discovery stays off unless configured, and lift the relay's refusal. Also
pass `allow_mtud = true` to `moq_quic::Endpoint::new`
(`rs/moq-uring/src/quic/driver/endpoint.rs`), which hardcodes `false`
today and would veto discovery for every connection whatever the setting;
it is sound only once the sockets refuse to fragment (`PMTUDISC_PROBE` plus
`IPV6_DONTFRAG`, from the send-errors quest), so land that first or set
them here. Reproduce first: a test path that
passes 1200-byte payloads but drops 1350 fails today and connects after.
Then cover discovery staying off by default, a path MTU change
mid-connection, and a larger MTU being discovered when enabled, with GSO
trains still batching.

Decided 2026-10-22 from the io_uring audit: m1, since it black-holes real
paths.

Public API: additive, a `mtu_discovery: bool` field on the
`#[non_exhaustive]` `moq_uring::quic::Transport`, named after
`moq_tokio::quic::Resolved::mtu_discovery`. Wire: none. Config:
`quic.mtu_discovery` works under `--runtime-io-uring`; update
`doc/bin/relay/` if it mentions the refusal.
