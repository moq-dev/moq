# [XL] QUIC backend bakeoff

## Goal

Choose the fastest greenfield foundation for the custom QUIC backend by
measuring `quiche`, `quinn-proto`, and `noq-proto` behind the same thread per
core io_uring relay. The verdict covers both relay efficiency and behavior
under bufferbloat. If no backend is Pareto-competitive across chat and media,
record no winner and keep quiche as the incumbent.

Scope is raw QUIC with moq-lite. Browser WebTransport is not part of the
comparison. The verdict creates a backend-specific raw-QUIC adoption quest;
WebTransport parity is a separate follow-up if the winner is not quiche.

## Plan

Existing evidence cannot choose: quiche has the strongest production record,
quinn-proto has the most promising owned receive and native GSO shape, and
noq-proto adds small-write coalescing and BBR3 but has mixed comparative
results. No primary-source benchmark runs all three on the same I/O runtime.

### Why not msquic

Considered and excluded as a fourth arm. msquic is not sans-IO: it owns its
sockets, buffers, and connection-ID steering. An adapter could share this rig's
ring and thread, because the preview `ExecutionCreate`/`ExecutionPoll` take an
app-supplied `QUIC_EVENTQ` that is a `struct io_uring` when built with
`QUIC_LINUX_IOURING_ENABLED`, but nothing below that. A fourth arm would be
confounded exactly where the shared worker removes variance, and it widens
every max-t family for a result this questline could not adopt: media-aware
transport features are the reason to own a backend, and on msquic each one is
C fork work with no rustls path.

Its io_uring datapath (`src/platform/datapath_iouring.c`) is still worth
reading as prior art for `moq-uring`: registered buffer rings, multishot
`recvmsg`, `UDP_SEGMENT` and `UDP_GRO`, and ordinary `sendmsg` with zero-copy
send left as a TODO. The datapath is opt-in, covered by BVT and sanitizer CI
but absent from msquic's own perf CI, so no published numbers describe it.

### Comparison

- Build benchmark-grade, conformance-tested `moq-uring` adapters for all three
  engines. Share the worker, socket, buffer ring, connection-ID steering,
  timers, offloads, flow-control limits, and MoQ workload. Each adapter uses
  its fastest credible owned-buffer and batching APIs plus its fastest
  production-ready crypto configuration; compare deployable ceilings, not a
  lowest-common-denominator API.
- Use adapter microbenchmarks and unscored pilots to choose those optimized
  paths. Before the first clean-path block, freeze every implementation,
  crypto/configuration bundle, workload manifest, and a shared finite capacity
  ladder whose adjacent loads differ by at most 5%. Every backend runs every
  rung through the same terminal load, with no data-dependent stop. The
  capacity comparison is resolved only when every backend has an observed
  failing rung above its highest passing rung. If the generator or NIC ceiling
  prevents that, upgrade the rig and repeat or record the capacity gate as
  unresolved and name no winner. A later correctness fix restarts the complete
  scored experiment.
- Honor each engine's native pacing contract. Quinn retains its owned
  `poll_transmit` pacing and GSO path; do not route it through the quiche
  executor experiment below.
- First force CUBIC to isolate engine efficiency. Before seeing any bufferbloat
  data, predeclare exactly one production controller per backend from
  independent primary-source deployment and implementation evidence. Gate it
  with the pinned media-plus-chat workload at 75 Mbps over a 100 Mbps, 50 ms
  path under both 1% independent random loss and a drop of three consecutive
  packets each second. Run ten matched blocks per profile, each with a two
  minute warmup and ten minute observation, and include the acceptance bounds
  in the network-stage family. The session must stay open, the media-goodput
  lower bound reach 95% of offered load, group-loss upper bound stay at or
  below 0.1%, p99 chat-probe
  upper bound stay at or below 250 ms, and post-warmup latency-slope upper bound
  stay at or below 1 ms/minute. Do not substitute another controller after a
  failure. Network-stage finalists run that pinned controller through the bufferbloat
  suite. Controller results do not replace the CUBIC attribution pass.
- Measure fixed-load CPU cost and a load ramp to maximum sustainable capacity.
  Primary costs are CPU per delivered chat message and CPU per delivered media
  byte. Also record goodput, p99 application latency, RSS, allocations, context
  switches, packet and offload counts, rate stability, group loss, and
  handshake latency. At every capacity-ramp point, run ten matched blocks with
  a two minute warmup and ten minute observation. Fit an ordinary least-squares
  slope to each run's RSS and queued-group samples collected once per second. A
  load is sustainable only when the clean-stage simultaneous upper bounds keep group
  loss at or below 0.1%, p99 delivery latency at or below 100 ms, RSS growth at
  or below 1 MiB/minute, and queued-group growth at or below 0.1% of offered
  groups per minute. Sustainable capacity is the highest load that passes.
- Name a winner only when one backend is not statistically worse on the primary
  CPU metric in any of the four delivery workloads and is statistically better
  in at least one. Run ten matched blocks per cell, randomizing backend order
  inside each block. Predeclare every winner comparison, then use a max-t
  bootstrap to construct simultaneous one-sided family-wise 97.5% bounds over
  all backend pairs, workloads, and quantitative gates within each stage.
  Allocate 2.5% error to the clean stage and 2.5% to the network stage so the
  complete winner decision has at least 95% family-wise coverage. Apply every
  margin to a bound, never a point estimate: against each
  other backend, the candidate's CPU upper ratio is at most 1.05 in every
  delivery workload, its sustainable-goodput lower ratio is at least 0.95, and
  its fixed-load p99-latency and RSS upper ratios are at most 1.10. At least one
  CPU upper ratio must be below 1.0. The fixed ten-block design has no
  data-dependent extension or early stop.
- The winner's production controller must also pass every bufferbloat depth:
  against every other finalist, its simultaneous media-goodput lower ratio is
  at least 0.95, p99 chat-probe latency upper ratio at most 1.10, and group-loss
  upper difference at most 0.1 percentage point. Fit each run's post-warmup
  chat-latency slope and require its simultaneous upper bound to be at most
  1 ms/minute. A split CPU result, unresolved bound, or failed bufferbloat gate
  records no winner. Exactly one backend must satisfy the complete rule;
  multiple qualifiers also record no winner. Extensibility for per-stream ACK
  telemetry, probing, custom congestion control, and FEC is reported
  qualitatively but does not select the winner.

### Workloads

Run every backend through the clean-path suite before eliminating any:

- tiny-message 1:1
- chat-room fanout
- media 1:1
- media fanout
- handshake churn
- idle-connection memory

Pin every `moq-bench` range and use the same offered load for every backend.
Tune each steady-state pilot once so the slowest backend consumes 60% to 70%
of one production worker, then freeze that load. Keep the existing chat, SD,
and HD presets as the source shapes rather than introducing a second load
generator.

Only clean-stage winner candidates run the bufferbloat suite. A backend remains
eligible only when its clean-stage simultaneous bounds satisfy every CPU,
sustainable-goodput, fixed-load p99, and RSS gate against every other backend,
including the strict CPU-superiority gate. If fewer than two remain eligible,
treat all three as network-stage finalists so the sole candidate still has a
comparison and a split or unresolved clean result is fully characterized.

Saturated media egress fills a 100 Mbps bottleneck with 50 ms base RTT while a
low-rate MoQ chat stream probes interactive delay on the same path. Sweep an
unshaped control and FIFO queues of 1, 4, and 16 bandwidth-delay products. Do
ten matched blocks at every depth, each with a two minute warmup and ten minute
observation; p99 and slope use only that fixed observation window. Do not add
unrelated competing traffic: the test asks how each backend's own controller
trades utilization against the queue it creates.

### Rig and artifacts

- Use loopback only for adapter microbenchmarks and offload ablations. Run the
  primary CPU comparison over a clean L2 path between an isolated relay-class
  Linux 6.12 or newer host and a separate generator, then run the queue sweep
  through reproducible netem namespaces on the generator. This keeps generator
  and shaping CPU out of the relay total. The NTP-synchronized generator needs
  at least twice the required packet and byte capacity.
- Hold the relay build, worker count and affinity, CPU governor, allocator,
  MTU, GSO/GRO settings, flow control, and a quinn/aws-lc `moq-bench` client
  fixed. Before the pilots, pin each engine's version and crypto provider and
  use AES-128-GCM everywhere. Do not upgrade during the experiment. The verdict
  is scoped to those deployable bundles and must be rechecked if adoption
  changes crypto. Restart the relay between repetitions.
- Extend the existing `moq-bench`, `moq-bench-host`, and `moq-uring` ablation
  reporting instead of creating a parallel harness. Emit raw JSONL plus a
  manifest containing commits, crate versions, crypto, congestion controller,
  hardware, kernel, workload, shaping, and repetition. Publish the raw
  artifacts and aggregate verdict with the verdict PR.
- Keep all three adapters through the verdict and adoption PRs so the result is
  reviewable and reproducible. The adoption PR removes the losers.

The existing quiche fork questline may proceed in parallel. If quiche wins or
the result is inconclusive, retain it. If another backend wins, the verdict PR
abandons that questline and creates the winner's raw-QUIC adoption quest plus a
separate WebTransport parity quest.

### Quiche pacing executor

Select and freeze quiche's production-ready transmit path before the first
clean-path block. Compare these executor shapes behind the same quiche adapter:

- userspace timer deadlines with same-release `UDP_SEGMENT` trains
- one `SCM_TXTIME` send per packet through io_uring, without GSO
- `SCM_TXTIME` plus GSO trains whose segments share one release timestamp

One GSO superpacket has one socket timestamp, so the last shape must split a
train whenever the destination or release time changes. Use quiche's
`ReleaseDecision` and `send_quantum()` rather than inventing an independent
pacer. Coalesce only packets quiche marks as burstable or time-equivalent and
never exceed its send quantum. The rationale and low-rate bounds come from the
[BBR send-quantum definition](https://datatracker.ietf.org/doc/html/draft-cardwell-ccwg-bbr#section-4.6.3),
but the adapter consumes the engine's value instead of duplicating the draft's
formula. Any wider timestamp quantization is a separately named candidate and
must pass the same scored quality rule.

msquic paces in userspace with `UDP_SEGMENT` trains and has no `SO_TXTIME`
path at all, so it is a production implementation of the first shape and
evidence for neither of the others.

Score every shape under both `fq` and strict `etf`, with matching clocks and no
deadline-mode relaxation. Treat executor plus qdisc as one candidate, then run
the full backend comparison under the selected qdisc. The rig must fail closed
when its Linux kernel, socket options, qdisc, or io_uring operations do not
support the candidate. There is no old-kernel fallback. Record the effective
qdisc hierarchy and counters, TX-queue count, offloads, and socket configuration
for every run; an unscored `fq_codel` run documents the current deployed
baseline but cannot select the executor. Because `fq` treats one multiplexed
worker socket as one flow, report whether cross-connection bursts change chat
delay.

Use BBRv2 as the primary pacing workload and repeat every correctness gate with
CUBIC. For each profile, run the existing fixed ten matched blocks with no
data-dependent extension. Hardware receive timestamps from the separate
generator are authoritative for on-wire packet spacing; retain sender TX
timestamps and qdisc statistics only for diagnosis. The `SCM_TXTIME` candidates
must use strict transmit times, drain `MSG_ERRQUEUE`, and count every missed
deadline and dropped packet; the userspace candidate counts late timer wakeups
against the same intended release schedule. Every candidate, including the
per-packet reference, must have zero post-warmup misses at every scored load or
the implementation or rig is not ready for the bakeoff.

The per-packet `SCM_TXTIME` path is the pacing-quality reference. A batching
candidate is eligible only when simultaneous one-sided max-t bounds prove
zero-margin non-inferiority: its candidate-minus-reference upper bound is at
most zero for p99 absolute inter-packet spacing error against quiche's release
schedule, p99 application latency, and group loss, while its lower bound is at
least zero for delivered goodput. This is evidence of no degradation, not a
failure to detect one. If no batching candidate passes, retain the per-packet
reference. Among eligible candidates, select a lower CPU per delivered byte
only with simultaneous evidence of an improvement; a tie retains the
reference.

After selecting that bundle, cross ordinary `SendMsg` with `SendMsgZc` as a
separate scored axis. Keep transmit storage alive through the notification CQE
and record the kernel's copied-versus-zero-copy usage report. Adopt zero-copy
only when the kernel reports actual zero-copy use, the simultaneous CPU bound
improves, and every pacing-quality bound still passes; otherwise retain the
ordinary send path.

This quest changes qdiscs only inside the benchmark rig. If quiche wins the
overall backend verdict, its adoption quest declares and verifies the selected
qdisc in Nix before production use.

## Related

- [Custom QUIC: the quiche fork](/quest/m2/quiche/README.md) - the incumbent
  plan and fallback when the bakeoff has no winner
- [moq#2875](https://github.com/moq-dev/moq/issues/2875) - the io_uring relay
  epic and its existing batching measurements
