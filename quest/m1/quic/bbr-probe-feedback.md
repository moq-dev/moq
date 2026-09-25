# [S] Finish BBR bandwidth-probe feedback once

## Goal

Finishing a bandwidth probe advances the bandwidth-history window once.
Later cruise losses are not classified as feedback from that probe.

## Plan

[adapt_long_term_model](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/congestion/bbr3/mod.rs#L991) leaves ack_phase at ProbeStopping and
bw_probe_samples set. Every subsequent cruise round can advance cycle_count,
and a later loss can reduce the long-term model as if it came from probing.
Compare [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1664),
[Google QUICHE](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr2_probe_bw.cc#L119), and the draft's
AdaptLongTermModel transition.

Exercise a completed loss-free probe followed by several cruise rounds.
Assert the filter advances only once and retains the intended probe-cycle
history. Inject a later cruise loss and verify only the appropriate
short-term response applies. Include application-limited feedback and
ProbeRTT entry so neither leaves stale probe classification. Land these
regressions in the fork's CI without changing public APIs or the wire.

## Related

- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - deliver the corrected controller to MoQ
- [Upstream the fork](/quest/m1/quic/upstream.md) - offer general fixes upstream
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - measure the corrected controller on media traffic
