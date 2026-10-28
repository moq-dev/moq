# [L] Drop web-transport-trait from the Rust stack

## Goal

moq-tokio, moq-uring, moq-relay, moq-ffi and moq-wasm no longer depend on
`web-transport-trait`. Native QUIC already implements moq-net's traits
([native WebTransport](/quest/m1/quic-api/native-web.md)); the iroh, wasm and ffi backends implement moq-net's
`transport::poll` traits, either directly or through a thin adapter per
backend. The generic adapters over `web_transport_trait` are deleted:
moq-tokio's `transport::Session<S>` with its async-to-poll emulation, and
moq-uring's `transport::Session<S>`, by whichever of this quest and
[native WebTransport](/quest/m1/quic-api/native-web.md) lands second. The
sealed `Grouped`/`GroupedSend` pair goes with them. A custom transport implements moq-net's traits, which are
already generic and send-group-aware (`Session::send_group`,
`SendStream::Group`, `Ungrouped`).

## Plan

Decided 2026-10-16 in the review of #5711: the extension point for custom
transports is moq-net's `transport::poll`, not `web_transport_trait`. #5711
sealed the adapters' bound to the crate's own backends, so a foreign
`web_transport_trait::Session` no longer fits them. This quest removes the
layer instead of reopening it.

- iroh gets its own impl. Flat backends use `Ungrouped`.
- The browser backend in moq-wasm and the ffi backend implement the traits
  directly. Their adapters already clamp priorities themselves.
- Report the public API impact per crate: deleted generic `Session<S>`
  adapters, and new impls on concrete session types.

Native QUIC, and with it aligning the two QUIC stacks, moved to m1 as
[native WebTransport](/quest/m1/quic-api/native-web.md) (maintainer,
2026-10-23); this quest keeps the remaining backends and the adapter
deletion.

Notes from related quests:
- The in-tree qmux already implements moq-net's `transport::poll`
  natively, so once native QUIC lands only iroh still goes through the adapter.
- [poll_acked](/quest/m2/quic-ack-hook.md) already puts its hook on moq-net's
  trait because moq-net has no `web-transport-trait` dependency. That holds
  here.
- [Native request admission](https://github.com/moq-dev/moq/pull/6409) inspects
  QUIC and qmux credit through moq-net's `poll_open_request`. Iroh keeps
  waiting, and browser-backed Rust retains its backend behavior. Move that
  admission hook into each backend's own moq-net impl.

Decided 2026-10-20 (maintainer): moq takes no new dependency on, or work in,
moq-dev/web-transport; `moq-quic` powers qmux and WebTransport. The iroh
(`web-transport-iroh`) and wasm (`web-transport-wasm`) backends stay crates
from that repo as accepted exceptions, and web-transport-proto may stay.

Decided 2026-10-21 (maintainer): web-transport-proto stays upstream, with no
migration quest. web-transport-iroh decodes CONNECT with it, so an in-tree
copy would duplicate the crate whenever iroh is on. Its QPACK bound ships as
the upstream web-transport-proto 0.6.3 patch.

Decided 2026-10-27 (maintainer): this waits only on moq-net's poll traits
taking `&self, &kio::Waiter`, which
[moq-dev/moq#7042](https://github.com/moq-dev/moq/pull/7042) landed, not on
[native WebTransport](/quest/m1/quic-api/native-web.md). The iroh, wasm, and
ffi impls need only that trait shape, so they need not wait for native
QUIC to move off the adapter. Whichever of native WebTransport and this quest
lands second deletes the generic adapters; native WebTransport already
expects to leave the adapter serving iroh. qmux uses `web-transport-trait`
only as a dev-dependency, for its 0.6 interop tests, so it does not block the
removal.

## Related

- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - another hook that lives only on moq-net's traits
