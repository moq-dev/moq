# moq-congestion

BBR3, Cubic, and Unlimited congestion controllers, consumed by moq-quic and the native transport drivers.

The controllers take explicit `moq_clock::Instant` values and never drive a runtime. The current callback traits retain QUIC packet-number spaces and a transport RTT snapshot; their portable redesign is planned separately.

The code was extracted from [moq-quic](../moq-quic), under the same MIT or Apache-2.0 license. Cubic originated in quinn; BBR3 was ported from moq-dev/noq and carries the fixes listed in [moq-quic's fork history](../moq-quic/README.md).

Controller tests run with `just rs test -p moq-congestion`. The cleanup benchmark runs nightly, or locally with:

```sh
just rs test --release -p moq-congestion -E 'test(benchmark_ack_cleanup)' --run-ignored only --success-output immediate
```

Connection characterization and simulated transport tests remain in moq-quic.
