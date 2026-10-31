# moq-congestion

BBR3, Cubic, and Unlimited congestion controllers, consumed by moq-quic and the native transport drivers.

The controllers take explicit `moq_clock::Instant` values and never drive a runtime. The `Controller` trait names no transport types: `on_sent` returns an opaque per-packet state that the transport stores and hands back with each acknowledgement or loss, and the controller paces its own sends.

Construct controllers with `Factory::build` on an `Arc<bbr3::Config>`, `Arc<cubic::Config>`, or `Arc<Unlimited>`. `Bbr3`, `Cubic`, `Unlimited`, and `Metrics` live at the crate root.

For callers of the unpublished API, replace `Bbr3Config` with `bbr3::Config`, `CubicConfig` with `cubic::Config`, `ControllerFactory` with `Factory`, and `ControllerMetrics` with `Metrics`. Remove `Controller::into_any` from custom implementations; inspect the controller's behavior or metrics instead of downcasting. `clone_box` still copies controller state for path migration. The test-only `Bbr3::probing_rtt` and `min_rtt` accessors are removed; model internals are tested in this crate.

The code was extracted from [moq-quic](../moq-quic), under the same MIT or Apache-2.0 license. Cubic originated in quinn; BBR3 was ported from moq-dev/noq and carries the fixes listed in [moq-quic's fork history](../moq-quic/README.md).

Controller tests run with `just rs test -p moq-congestion`. The per-packet benchmark runs nightly, or locally with:

```sh
just rs test --release -p moq-congestion -E 'test(benchmark_per_packet)' --run-ignored only --success-output immediate
```

Connection characterization and simulated transport tests remain in moq-quic.
