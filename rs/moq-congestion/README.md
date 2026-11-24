# moq-congestion

BBR3, Cubic, and Unlimited congestion controllers, consumed by moq-quic and the native transport drivers. Applications pick one through `moq_tokio::quic::CongestionControl`, by algorithm name: `bbr` (BBR3, the default), `cubic`, or `unlimited`.

Candidates still under test compile only with the `lab` test-support feature, which the comparisons in moq-quic and moq-tokio enable as a dev-dependency: the `gcc` and `scream` modules, and BBR3's delay bound (`bbr3::Config::delay_bound`), which brakes when the peer's receive timestamps show the bottleneck queue growing. A winner joins `CongestionControl` as a new variant, or BBR3 as an option, and leaves the feature.

The controllers take explicit `kio::clock::Instant` values and never drive a runtime. The `Controller` trait names no transport types: `on_sent` returns an opaque per-packet state that the transport stores and hands back with each acknowledgement or loss, and the controller paces its own sends.

Construct controllers with `Factory::build` on an `Arc<bbr3::Config>`, `Arc<cubic::Config>`, or `Arc<Unlimited>`, and with `lab`, `Arc<gcc::Config>` or `Arc<scream::Config>`. `Unlimited` and `Metrics` live at the crate root, and `gcc::Gcc` and `scream::Scream` in their modules.

The code was extracted from [moq-quic](../moq-quic), under the same MIT or Apache-2.0 license. Cubic originated in quinn; BBR3 was ported from moq-dev/noq and carries the fixes listed in [moq-quic's fork history](../moq-quic/README.md). SCReAMv2's delay-based path was ported from draft-ietf-ccwg-rfc8298bis-screamv2 and checked against Ericsson's [reference implementation](https://github.com/EricssonResearch/scream).

GCC is libwebrtc's Google Congestion Control, a delay-based controller that reads the peer's receive timestamps (draft-ietf-quic-receive-ts). `src/gcc` vendors a Rust port of WebRTC's `goog_cc` under WebRTC's BSD license, kept in [`src/gcc/LICENSE`](src/gcc/LICENSE) with its `PATENTS` grant; its module docs record the WebRTC revision it tracks and what it trims.

Controller tests run with `just rs test -p moq-congestion`, and the candidates' with `--features lab`. The per-packet benchmark runs nightly, or locally with:

```sh
just rs test --release -p moq-congestion --features lab -E 'test(benchmark_per_packet)' --run-ignored only --success-output immediate
```

Connection characterization and simulated transport tests remain in moq-quic.
