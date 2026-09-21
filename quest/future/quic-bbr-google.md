# [M] Measure the remaining Google BBR differences

## Goal

A measured decision on two remaining differences from Google's public BBR:
recognizing bandwidth growth within a round and precautionary bandwidth
probing. Each gets an adopt, retain, or investigate-further verdict; Google
parity is not assumed to be an improvement and does not gate correctness fixes.

## Plan

Use the corrected fork as the baseline. The 2026-09-21 audit compared
noq-proto 1.3.0 / upstream `1a26a8b` with Google Linux BBRv3 `90210de4`
and Google QUICHE `535a2730`. Recheck current upstream sources before testing.
Use QUICHE's actual `bbr3_sender.cc` with its shared `bbr2_*` model;
the older BBR2 sender is not a substitute for its BBR3 state machine.

- [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1924) recognizes growth on any ACK and increments
  the plateau counter only at a round boundary. Noq follows
  [draft-06](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.3.1.2)'s earlier round-start gate. QUICHE
  instead checks its bandwidth maximum at round boundaries. Test bursty and
  aggregated ACKs and changing bottleneck capacity for premature plateau exits.
- [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1803) and
  [Google QUICHE](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_sender.cc#L1079) stop precautionarily when
  probing reaches a previously lossy inflight bound, then accelerate a later
  probe if feedback is clean. Noq lacks this state and transition. Compare
  shallow queues, capacity increases, random loss, and competing flows.

Report throughput, queue delay, loss, convergence time, and fairness with
pinned code and configurations. Keep transport differences and QUICHE flags
explicit; compare each algorithm change separately. A simulation result is
not an end-to-end network measurement. Persist a reproducible harness in CI
(at least nightly) and the verdict with the quest's completion. Create a
separate implementation quest for any adopted change rather than silently
expanding this study into a controller rewrite.

## Required

- [Release BBR fixes](/quest/next/quic/bbr-release.md) - measure a baseline without the seven known defects

## Related

- [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) - reuse media profiles and measurements
- [Upstream the fork](/quest/next/quic/upstream.md) - share useful findings with upstream
