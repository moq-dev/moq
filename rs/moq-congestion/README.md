# moq-congestion

BBR3, Cubic, GCC, and Unlimited congestion controllers, consumed by moq-quic and the native transport drivers.

The controllers take explicit `kio::clock::Instant` values and never drive a runtime. The `Controller` trait names no transport types: `on_sent` returns an opaque per-packet state that the transport stores and hands back with each acknowledgement or loss, and the controller paces its own sends.

The code was extracted from [moq-quic](../moq-quic), under the same MIT or Apache-2.0 license. Cubic originated in quinn; BBR3 was ported from moq-dev/noq and carries the fixes listed in [moq-quic's fork history](../moq-quic/README.md).

GCC is libwebrtc's Google Congestion Control, a delay-based controller that reads the peer's receive timestamps (draft-ietf-quic-receive-ts). `src/gcc` vendors a Rust port of WebRTC's `goog_cc` under WebRTC's BSD license, kept in [`src/gcc/LICENSE`](src/gcc/LICENSE) with its `PATENTS` grant; its module docs record the WebRTC revision it tracks and what it trims.

Controller tests run with `just rs test -p moq-congestion`. The per-packet benchmark runs nightly, or locally with:

```sh
just rs test --release -p moq-congestion -E 'test(benchmark_per_packet)' --run-ignored only --success-output immediate
```

Connection characterization and simulated transport tests remain in moq-quic.
