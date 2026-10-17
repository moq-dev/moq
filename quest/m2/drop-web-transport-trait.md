# [L] Drop web-transport-trait from the Rust stack

## Goal

moq-tokio, moq-uring, moq-relay, moq-ffi and moq-wasm no longer depend on
`web-transport-trait`. The qmux, iroh and wasm backends implement moq-net's
`transport::poll` traits, either directly or through a thin adapter per
backend. The generic adapters over `web_transport_trait` are deleted:
moq-tokio's `transport::Session<S>` with its async-to-poll emulation, and
moq-uring's `transport::Session<S>`. The sealed `Grouped`/`GroupedSend` pair
goes with them. A custom transport implements moq-net's traits, which are
already generic and send-group-aware (`Session::send_group`,
`SendStream::Group`, `Ungrouped`).

## Plan

Decided 2026-10-16 in the review of #5711: the extension point for custom
transports is moq-net's `transport::poll`, not `web_transport_trait`. #5711
sealed the adapters' bound to the crate's own backends, so a foreign
`web_transport_trait::Session` no longer fits them. This quest removes the
layer instead of reopening it.

- Native QUIC (`moq_tokio::quic::web`, `moq_uring::quic::web`) implements
  `poll::Session` itself, keeping its real send groups. It is a candidate for
  the first step, since its `web_transport_trait` impl only feeds the adapter.
- qmux and iroh get their own impls. Flat backends use `Ungrouped`.
- The browser backend in moq-wasm and the ffi backend implement the traits
  directly. Their adapters already clamp priorities themselves.
- Report the public API impact per crate: deleted generic `Session<S>`
  adapters, and new impls on concrete session types.

Notes from related quests:
- [qmux on moq-quic](/quest/m2/quic-qmux.md) planned to port qmux's
  `web-transport-trait` impl. If it lands first, its impl targets moq-net's
  `transport::poll` instead.
- [poll_acked](/quest/m2/quic-ack-hook.md) already puts its hook on moq-net's
  trait because moq-net has no `web-transport-trait` dependency. That holds
  here.
- [Rust request credit](/quest/m1/rs-request-credit.md) prefers adding a
  credit query to qmux and iroh upstream in moq-dev/web-transport. Once this
  lands, that query belongs on each backend's own moq-net impl.

## Related

- [qmux on the QUIC stream state machine](/quest/m2/quic-qmux.md) - the in-tree qmux implements the moq-net traits directly
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - another hook that lives only on moq-net's traits
- [Rust requests fail fast without stream credit](/quest/m1/rs-request-credit.md) - its credit query moves to each backend's moq-net impl
