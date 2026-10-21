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

The two native QUIC stacks drifted apart behind the adapters. Align them while
each gets its own impl:

- moq-tokio's `quic::web::Session` derefs to `quic::driver::Connection`, so it
  has two `send_datagram`s with different framing: its own prefixes the
  WebTransport session id, the driver's reached through `Deref` does not.
  Prefer dropping the `Deref` over documenting the trap.
- The driver stream setters disagree: moq-tokio's `set_priority`/`set_group`
  take `&self` and return `Result<(), ClosedStream>`, moq-uring's take
  `&mut self` and return nothing.
- The sealed `Grouped::send_group` takes `&self` in moq-tokio and `&mut self`
  in moq-uring, and only moq-tokio requires `GroupedSend::Group: Send + Sync`.
- moq-uring's driver opens streams only through `web_transport_trait`, so
  `SendOptions` reach a new stream through its setters in both runtimes'
  adapters. A native impl can pass them to the open, as moq-tokio's driver
  `open_uni`/`open_bi` already accept.
- moq-tokio's `quic::web` module is public but `#[doc(hidden)]`. Decide
  whether it is API or should be private. `quic::driver` stays public:
  [An async QUIC driver without moq-net](/quest/m1/quic-driver.md) makes it
  supported API.

Notes from related quests:
- [qmux on moq-quic](/quest/m1/quic-qmux.md) planned to port qmux's
  `web-transport-trait` impl. If it lands first, its impl targets moq-net's
  `transport::poll` instead.
- [poll_acked](/quest/m2/quic-ack-hook.md) already puts its hook on moq-net's
  trait because moq-net has no `web-transport-trait` dependency. That holds
  here.
- [Rust request credit](/quest/m1/rs-request-credit.md) gets qmux's credit
  query from the in-tree qmux; iroh keeps waiting. Once this lands, that
  query belongs on each backend's own moq-net impl.

Decided 2026-10-20 (maintainer): moq takes no new dependency on, or work in,
moq-dev/web-transport; `moq-quic` powers qmux and WebTransport. The iroh
(`web-transport-iroh`) and wasm (`web-transport-wasm`) backends stay crates
from that repo as accepted exceptions, and web-transport-proto may stay.

## Related

- [qmux on the QUIC stream state machine](/quest/m1/quic-qmux.md) - the in-tree qmux implements the moq-net traits directly
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - another hook that lives only on moq-net's traits
- [Rust requests fail fast without stream credit](/quest/m1/rs-request-credit.md) - its credit query moves to each backend's moq-net impl
