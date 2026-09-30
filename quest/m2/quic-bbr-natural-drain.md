# [L] BBR media study

## Goal

Measured adopt, retain, or investigate-further decisions on three BBR
behaviors under media traffic: allowing natural media drains to satisfy
ProbeRTT, recognizing bandwidth growth within a round, and precautionary
bandwidth probing. Reduce demonstrated frame deadline interference without
stale minimum RTT, persistent queues, or unfairness. Application limitation
alone is not evidence that the path drained, and Google parity is not
assumed to be an improvement.

## Plan

Decided in the 2026-09-30 audit: the Google BBR comparison merged here as
one study, since both reuse the same media profiles, harness, and baseline.

### Natural drains

Use the corrected released fork as baseline. ProbeRTT targets half the
estimated BDP, floored at the minimum pipe window; entering the mode need
not withhold useful bytes. First locate actual cwnd stalls and deadline
misses using steady low-rate traffic, 30-fps frames with keyframes, and gaps
shorter and longer than RTT. Include both learned capacity above the media
rate and a sender that has never measured more than its source rate.

Compare the baseline with Google's explicit idle accounting and bounded
natural-drain credit. [QUICHE BBR3](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_sender.cc#L534)
postpones RTT aging across zero-inflight idle intervals; its
[tests](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_simulator_test.cc#L1724)
cover restart behavior. These source defaults are not deployment evidence.
Noq's Linux-style idle-restart suppression is narrower.

A candidate credits sustained low inflight, a completed sampled round, and
fresh RTT evidence toward ProbeRTT. Determine the predicate experimentally;
neither the app-limited label nor a stale BDP threshold is sufficient. Bound
deferral and retain fallback after stale evidence, increased base RTT, or a
path change. Blanket skipping is only a diagnostic control. Retain the
simpler baseline if no material interference is demonstrated.

Use identical media traces and seeds. Start with short and long RTT paths,
then test capacity changes, base-RTT changes, competing bulk traffic, shallow
queues, loss bursts, and compressed or delayed ACKs. Record deadlines and
frame completion, withheld useful bytes, sender backlog separately from
network queueing, RTT-baseline error, sample labels, and mode residence.
Set latency and fairness acceptance limits before tuning. Persist the
harness in CI, with broader network cases at least nightly, and a verdict
with pinned sources/configurations. Any adopted production policy gets a
separate implementation quest; this study does not silently change defaults.

### Google differences

The 2026-09-21 audit compared noq-proto 1.3.0 / upstream `1a26a8b` with
Google Linux BBRv3 `90210de4` and Google QUICHE `535a2730`. Recheck current
upstream sources before testing. Use QUICHE's actual `bbr3_sender.cc` with
its shared `bbr2_*` model; the older BBR2 sender is not a substitute for its
BBR3 state machine.

- [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1924)
  recognizes growth on any ACK and increments the plateau counter only at a
  round boundary. Noq follows
  [draft-06](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.3.1.2)'s
  earlier round-start gate. QUICHE instead checks its bandwidth maximum at
  round boundaries. Test bursty and aggregated ACKs and changing bottleneck
  capacity for premature plateau exits.
- [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1803)
  and [Google QUICHE](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_sender.cc#L1079)
  stop precautionarily when probing reaches a previously lossy inflight
  bound, then accelerate a later probe if feedback is clean. Noq lacks this
  state and transition. Compare shallow queues, capacity increases, random
  loss, and competing flows.

Report throughput, queue delay, loss, convergence time, and fairness with
pinned code and configurations. Keep transport differences and QUICHE flags
explicit and compare each algorithm change separately. A simulation result
is not an end-to-end network measurement.

## Related

- [Upstream the fork](/quest/m1/quic/upstream.md) - share useful findings with upstream
- [Discover media headroom](/quest/m2/quic-probe.md) - preserving an estimate and discovering spare capacity are separate problems
