# moq-congestion

BBR3, Cubic, and Unlimited congestion controllers, consumed by moq-quic and the native transport drivers. Applications pick one through `moq_tokio::quic::CongestionControl`, where `delay` is BBR3 and the default.

Candidates still under test stay off the published API: GCC and SCReAMv2 live in the workspace-only [moq-congestion-lab](../moq-congestion-lab), and BBR3's delay bound, which brakes when the peer's receive timestamps show the bottleneck queue growing, is settable only with the `lab` test-support feature those comparisons enable. A winner joins `CongestionControl` as a new variant, or BBR3 as an option.

The controllers take explicit `kio::clock::Instant` values and never drive a runtime. The `Controller` trait names no transport types: `on_sent` returns an opaque per-packet state that the transport stores and hands back with each acknowledgement or loss, and the controller paces its own sends.

Construct controllers with `Factory::build` on an `Arc<bbr3::Config>`, `Arc<cubic::Config>`, or `Arc<Unlimited>`. `Unlimited` and `Metrics` live at the crate root.

The code was extracted from [moq-quic](../moq-quic), under the same MIT or Apache-2.0 license. Cubic originated in quinn; BBR3 was ported from moq-dev/noq and carries the fixes listed in [moq-quic's fork history](../moq-quic/README.md).

Controller tests run with `just rs test -p moq-congestion`. The per-packet benchmark runs nightly, or locally with:

```sh
just rs test --release -p moq-congestion -E 'test(benchmark_per_packet)' --run-ignored only --success-output immediate
```

Connection characterization and simulated transport tests remain in moq-quic.
