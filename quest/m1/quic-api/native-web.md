# [L] Native WebTransport implements moq-net's traits

## Goal

`moq_tokio::quic::web` and `moq_uring::quic::web` implement moq-net's
`transport::poll` traits directly over their drivers' poll forms, keeping
real send groups. Their `web_transport_trait` impls, the sealed
`Grouped`/`GroupedSend` pair, and the generic adapters over
`web_transport_trait` are gone: moq-tokio's async-to-poll
`transport::Session<S>` and moq-uring's `transport::Session<S>`, which serve
only native QUIC now that iroh, moq-wasm and moq-ffi implement moq-net's
traits themselves. moq-tokio, moq-uring and moq-relay then no longer depend on
`web-transport-trait`; qmux keeps it as a dev-dependency for its 0.6 interop
tests, and web-transport-iroh still pulls it in transitively.

## Plan

Moved from drop web-transport-trait (maintainer, 2026-10-23): the adapter
exists because quinn's driver was async-only, so it goes with the rest of
quinn's shape before the cut.

What it deletes, from the 2026-10-23 consumer audit (about 400 lines):
moq-tokio's `transport.rs` QUIC paths (a boxed future per operation, a
`Bytes::copy_from_slice` per write and datagram, `finish` and `reset`
deferred until an in-flight operation settles, an emulated `closed()`),
`grouped.rs`, the QUIC glue in `transport.rs` and `owned.rs` (the `Lease`
impl and `close_kind` downcasting through `web::ReadError`), the
`web_transport_trait` impls in `web/session.rs`, `web/send.rs`, and
`web/recv.rs`, the `i32` priority clamp, and the `futures::stream::unfold`
around `accept_uni`. `session.rs`'s `finish()` then `stopped().await` becomes
awaiting `send::Finished`.

Align the two stacks while each gets its impl:

- moq-tokio's `quic::web::Session` derefs to `quic::driver::Connection`, so
  it has two `send_datagram`s with different framing: its own prefixes the
  WebTransport session id, the driver's does not. Drop the `Deref`.
- The session id is chained onto each datagram with `send(impl Buf)`
  ([moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)) instead of allocated and copied
  per datagram (quinn-rs/quinn#1724).
- `quic::web` is supported API in both crates (maintainer, 2026-10-23): a
  WebTransport-over-moq-quic library usable without MoQ, documented, no
  longer `#[doc(hidden)]`, with the same names and signatures in moq-tokio
  and moq-uring and the driver's `send::`/`recv::` shapes and errors.
- moq-uring's header classification (`classify_uni`, `classify_bi` in
  `web.rs`) re-polls every pending stream and rebuilds the pending `Vec`
  whenever any stream arrives or wakes, so a peer that opens N streams with
  partial headers costs O(N²) CPU, with N up to its stream limit. moq-tokio
  keeps them in a `FuturesUnordered`, which polls only the woken ones. Bring
  uring to the same shape. Test with pending × ready headers and fragmented
  arrival, preserving FIN handling, the held control streams, and stream
  credit (2026-10-27 audit, maintainer).
- `transport::poll_op` and `OpSlot` move to `moq_tokio::iroh`, their last
  user, when moq-tokio's adapter goes.
- moq-relay's io_uring server and its tests call the uring session through
  `web_transport_trait` (`uring.rs`, `tests/integration/runtime_uring.rs`);
  move them to the new impls.
- Report the public API impact per crate: deleted `Grouped`/`GroupedSend`,
  `web_transport_trait` impls, and generic `transport::Session<S>` adapters,
  and new `poll::Session` impls.

Measure relay CPU per Gbps and allocations per stream before and after on
the same workload.

## Required

- [Poll forms](/quest/m1/quic-api/poll.md) - the leak-free driver polls these impls forward to

## Related

- [Relay allocations](/quest/m2/perf/relay-allocs.md) - retakes its profile after the adapter's per-stream boxing is gone
