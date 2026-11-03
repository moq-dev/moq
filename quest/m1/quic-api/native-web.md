# [L] Native WebTransport implements moq-net's traits

## Goal

`moq_tokio::quic::web` and `moq_uring::quic::web` implement moq-net's
`transport::poll` traits directly over their drivers' poll forms, keeping
real send groups. Their `web_transport_trait` impls, the sealed
`Grouped`/`GroupedSend` pair, and the generic adapters over
`web_transport_trait` are gone: moq-tokio's async-to-poll
`transport::Session<S>` and moq-uring's `transport::Session<S>`, which serve
only native QUIC. moq-tokio, moq-uring and moq-relay then no longer depend on
`web-transport-trait`; qmux keeps it as a dev-dependency for its 0.6 interop
tests.

## Plan

The native trait migration is implemented on this quest's branch. Finish its
validation before removing this quest:

- Run local checks and the cross-runtime stream, FIN, send-group and receive-credit
  tests on the final committed head. The empty-FIN close watch fails on the
  driver-poll base and must pass on both stacks.
- Run the io_uring `web_headers` pending × ready sweep. It covers uni and bi
  headers, later fragmented completion, preserved payloads and FIN, and stream
  credit returning after consumption. The classifier polls only woken entries;
  three HTTP/3 critical-stream slots replace the old retained-stream cap.
- Measure relay CPU per Gbps and allocations per stream before and after, with
  the same workload and disclosed host load. `fanout` also measures the native
  poll interface directly with allocation counters (`NATIVE_ALLOCS=1`).
- Review the checked head locally and address findings before readiness.

The supported common API uses `web::send::Stream`, `web::recv::Stream`, their
role-local errors, consuming terminal methods, and `send::Finished` futures.
Sessions implement moq-net's poll traits directly with real send groups;
io_uring retains only `transport::Local` for moq-net's thread bound.
Both handshakes use `proto::ConnectResponse` and HTTP status codes. Connection
access is explicit, rather than through the Tokio session's `Deref`.

The shared strict-flake runner failure writing `/proc/self/uid_map` is an
existing environment limitation. Preserve the probe/skip policy for ordinary
checks and the intentional failure under strict CI, and record base/head
failures without changing the harness to pass.

## Related

- [Relay allocations](/quest/m2/perf/relay-allocs.md) - retakes its profile after the adapter's per-stream boxing is gone
