# moq-congestion

BBR3, Cubic, SCReAMv2, and Unlimited congestion controllers, consumed by moq-quic and the native transport drivers.

The controllers take explicit `kio::clock::Instant` values and never drive a runtime. The `Controller` trait names no transport types: `on_sent` returns an opaque per-packet state that the transport stores and hands back with each acknowledgement or loss, and the controller paces its own sends.

The code was extracted from [moq-quic](../moq-quic), under the same MIT or Apache-2.0 license. Cubic originated in quinn; BBR3 was ported from moq-dev/noq and carries the fixes listed in [moq-quic's fork history](../moq-quic/README.md). SCReAMv2's delay-based path was ported from draft-ietf-ccwg-rfc8298bis-screamv2 and checked against Ericsson's [reference implementation](https://github.com/EricssonResearch/scream).

Controller tests run with `just rs test -p moq-congestion`. The per-packet benchmark runs nightly, or locally with:

```sh
just rs test --release -p moq-congestion -E 'test(benchmark_per_packet)' --run-ignored only --success-output immediate
```

Connection characterization and simulated transport tests remain in moq-quic.
