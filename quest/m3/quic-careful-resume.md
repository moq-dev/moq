# [M] Careful resume on reconnect

## Goal

A QUIC connection to a host the process recently talked to starts at the
previous QUIC connection's delivered rate instead of the initial window.
A relay reconnect after a peer restart and a redial after a GOAWAY reach their
steady rate in one RTT, not a slow start. A stale or wrong estimate falls back
to slow start without hurting the path.

## Plan

Decided in the 2026-10-10 audit: scope this to remembered QUIC connections.

Follow the shape of draft-ietf-ccwg-careful-resume: the sender keeps a
per-destination record of the last validated bandwidth estimate and minimum
RTT with its age, jumps the congestion window toward that estimate once the
new path's RTT matches the recorded one, and drops back to ordinary slow
start on the first loss or a mismatched RTT.

- Implement in `moq-quic` as a `Controller` wrapper: any controller can be
  resumed. BBR3 seeds its bandwidth model and pacing rate directly; Cubic
  seeds `ssthresh`.
- The store is a bounded, in-process map keyed by remote address plus SNI,
  owned by the endpoint, with an age limit. No persistence across processes.
- moq-tokio's `Connection` (its reconnect loop and GOAWAY handover in
  `rs/moq-tokio/src/connection.rs`, dialing through the race in
  `failover.rs`) seeds a redial from the session it replaces, and
  the relay's cluster peers seed from the previous session to the same peer.

Measure time to the encoder's target rate after a reconnect on bufferbloat:
a rate below the media bitrate behind a deep but finite queue, with actual
overflow drops, plus loss and latency during the jump. Include competing
loss-based traffic when BBR alone does not fill the queue. Random loss may
prove fallback correctness, but never grades the jump's performance.
Ship it on by default
only when the jump never makes the first second worse than slow start.
