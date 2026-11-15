# moq-congestion-lab

Congestion controller candidates under test, kept out of [moq-congestion](../moq-congestion)'s published API. The crate is never published: moq-quic's simulated comparisons and moq-tokio's real-socket tests take it as a path dev-dependency, which `cargo publish` drops. A candidate that wins the realtime bake-off moves into moq-congestion and joins `moq_tokio::quic::CongestionControl` as a variant named after its algorithm.

SCReAMv2's delay-based path was ported from draft-ietf-ccwg-rfc8298bis-screamv2 and checked against Ericsson's [reference implementation](https://github.com/EricssonResearch/scream).

GCC is libwebrtc's Google Congestion Control, a delay-based controller that reads the peer's receive timestamps (draft-ietf-quic-receive-ts). `src/gcc` vendors a Rust port of WebRTC's `goog_cc` under WebRTC's BSD license, kept in [`src/gcc/LICENSE`](src/gcc/LICENSE) with its `PATENTS` grant; its module docs record the WebRTC revision it tracks and what it trims.

Tests run with `just rs test -p moq-congestion-lab`.
