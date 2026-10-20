# [M] QUIC receive-timestamps spike

## Goal

`moq-quic` implements draft-ietf-quic-receive-ts-03 between native peers, and a
written verdict says whether its forward delay gradient detects uplink queue
growth earlier and with less noise than the RTT gradient, on cellular-like
moq-shaper presets. The extension lands only on a positive verdict, for the
candidates in [the realtime epic](/quest/m2/quic-realtime/README.md); a
negative verdict sends them to RTT gradients instead.

## Plan

Decided 2026-10-17: moved from m3 into the realtime uplink epic, and re-aimed
from relay egress to native publisher ingest. Both ends are ours.

- Wire: the `max_receive_timestamps_per_ack` (0x4ac07) and
  `receive_timestamps_exponent` (0x4ac26) transport parameters and the
  `ACK_RECEIVE_TIMESTAMPS` frame (0x03178307..0x03178308) with timestamp
  ranges. Skip `PATH_ACK_RECEIVE_TIMESTAMPS`; `moq-quic` has no multipath.
  Only 1-RTT packets carry it, and a peer that did not advertise a non-zero
  maximum never receives it.
- Receiver: fill spare ACK space with the newest receive times first, as the
  draft's section 7.2 recommends; never send a packet just for timestamps.
- Receive time: kernel `SO_TIMESTAMPNS` where the OS has it (moq-sock
  already sets it and exposes `RecvMeta::timestamp`, used by moq-tokio and
  moq-uring), userspace recv instant elsewhere. A GRO batch reports only its
  first datagram and omits the rest (section 7.1), since one kernel stamp
  covers the whole batch.
- The kernel stamp is `CLOCK_REALTIME`, which NTP or an admin can step. Map
  it onto the monotonic clock with the realtime-to-monotonic offset sampled
  at each recv, and when the offset jumps between batches, drop that batch's
  kernel stamps for the userspace instant, so a clock step never reads as
  queue growth or drain. The basis must stay constant (section 6.1).
- Sender: `congestion::Controller` sees each acknowledged packet's send
  instant and peer receive instant, so every candidate reads the same input.
- Receive times measure delay variation, not absolute one-way delay: the
  receiver's basis is never communicated (section 6.1). State clock offset
  and drift assumptions; never label raw receive times absolute delay.
- Harness: add cellular-like uplink presets to `rs/moq-shaper/profiles`
  (rate steps down and up, bursty capacity, deep buffer) for the spike and
  the candidates to share. Report the shaper's own timing jitter as the noise
  floor beside every delay result.
- Measure: queue-growth detection latency and false positives for the
  receive-ts gradient versus the RTT gradient, and the ACK bytes the
  timestamps add on the publisher's downlink, with and without the
  ACK-frequency extension.

The shaper is userspace: state beside the result that it cannot establish
behavior against production cross traffic or real wifi and cellular loss.

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - can reuse the forward-delay estimate
