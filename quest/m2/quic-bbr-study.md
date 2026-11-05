# [M] BBR media study

## Goal

Measured adopt, retain, or investigate-further decisions on two BBR
behaviors under media traffic: recognizing bandwidth growth within a round,
and precautionary bandwidth probing. Reduce demonstrated frame deadline
interference without persistent queues or unfairness. Google parity is not
assumed to be an improvement. Acceptance and performance use a rate below
the media bitrate behind a deep but finite queue with verified overflow
drops. Random loss may prove recovery correctness, but never grades media
quality or performance.

## Plan

Decided in the 2026-09-30 audit: the Google BBR comparison merged here as
one study, since both reuse the same media profiles, harness, and baseline.
ProbeRTT natural-drain credit is settled: BBR3 skips a due ProbeRTT after an
interval that was app-limited at least once and saw no loss or CE (QUICHE
BBRv1's kBBR8, which subsumes its near-floor kBBR7). The maintainer chose
kBBR8 on 2026-10-25 on the advice of BBR's designer, replacing the
2026-10-24 kBBR7-only decision. The rejected risk, `min_rtt` ratcheting up
under a standing queue the flow builds itself, is the known cost; a skip keeps
the `min_rtt` value rather than taking the queued samples, so it shows up as
the mirror image: while media stays app-limited and congestion-free, `min_rtt`
does not follow a base-RTT rise until a lost or CE-marked packet, or an
interval with no starved poll, brings ProbeRTT back.

### Harness

Use `moq-congestion`'s BBR3, which carries the released fork's corrections,
as baseline, with 30-fps frames and keyframes. Include both learned capacity
above the media rate and a sender that has never measured more than its
source rate.

Use identical media traces and seeds. Start with short and long RTT paths,
then test capacity changes, base-RTT changes, competing bulk traffic, queue
depths, and compressed or delayed ACKs. Use loss-based cross traffic when
BBR alone does not fill the deep queue; verify actual overflow drops. Keep
scripted loss bursts as recovery correctness checks only. Record deadlines and
frame completion, sender backlog separately from network queueing, sample
labels, and mode residence.
Set latency and fairness acceptance limits before tuning. Persist the
harness in CI, with broader network cases at least nightly, and a verdict
with pinned sources/configurations. Any adopted production policy gets a
separate implementation quest; this study does not silently change defaults.

Check which side sends keep-alives in moq-tokio's native QUIC config. If
both sides ping, the sender's controller may see no samples during an idle,
which masks bugs like #4219. BBR also idles in its 1.25x probe-bandwidth
phase while the app has nothing to send; this is harmless today because
restart from idle resets the gain to 1.0.

### Google differences

The 2026-09-21 audit compared noq-proto 1.3.0 / upstream `1a26a8b` with
Google Linux BBRv3 `90210de4` and Google QUICHE `535a2730`. Recheck current
upstream sources before testing. The audit describes noq; `moq-quic`'s BBR3
was ported from it (#4921), so recheck each difference against `moq-congestion`.
Use QUICHE's actual `bbr3_sender.cc` with
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
  state and transition. Compare queue depths, capacity increases, and
  competing flows on shaped paths with verified overflow drops.
- Found in [moq-dev/moq#7017](https://github.com/moq-dev/moq/pull/7017):
  `moq-congestion`'s extra-acked filter is a 10-round Nichols `MaxFilter`
  (`EXTRA_ACKED_FILTER_LEN` in `rs/moq-congestion/src/bbr3/mod.rs`), while
  Google Linux keeps two slots of 5 rounds each, the shape #7017 gave the
  bandwidth filter. Compare aggregation headroom and queue delay under
  bursty ACKs before changing it.

Report throughput, queue delay, loss, convergence time, and fairness with
pinned code and configurations. Keep transport differences and QUICHE flags
explicit and compare each algorithm change separately. A simulation result
is not an end-to-end network measurement.

### Congestion evidence

Use the overflowing and deep-queue lanes in `test/ts/README.md` as the
end-to-end baseline. A BBR-only run that sheds groups at the relay but never
overflows the network queue cannot judge the loss response. A CUBIC run
that overflows establishes that the harness exercises recovery; it cannot
establish whether changing BBR improves it.

Keep the current 2% `LOSS_THRESH` and baseline controller policy unless a
candidate improves the shaped-path comparison, including queues and
fairness. Surviving an independent random-loss percentage is not a reason
to weaken the congestion response or precautionary probe stop. This study
still decides growth recognition and probe behavior separately; it does not
adopt either in advance.

## Related

- [TS validation](/test/ts/README.md) - shaped paths, whole-group shedding, and measured baselines
