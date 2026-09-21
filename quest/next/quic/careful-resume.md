# [M] Careful resume on reconnect

## Goal

A connection to a host the process recently talked to starts at the previous
connection's delivered rate instead of the initial window. A relay reconnect
after a peer restart, a client's WebSocket-to-QUIC upgrade, and a redial after
a GOAWAY reach their steady rate in one RTT, not a slow start. A stale or wrong
estimate falls back to slow start without hurting the path.

## Plan

Follow the shape of draft-ietf-ccwg-careful-resume: the sender keeps a
per-destination record of the last validated bandwidth estimate and minimum
RTT with its age, jumps the congestion window toward that estimate once the
new path's RTT matches the recorded one, and drops back to ordinary slow
start on the first loss or a mismatched RTT.

- Implement in the fork as a `Controller` wrapper: any controller can be
  resumed. BBR3 seeds its bandwidth model and pacing rate directly; Cubic
  seeds `ssthresh`.
- The store is a bounded, in-process map keyed by remote address plus SNI,
  owned by the endpoint, with an age limit. No persistence across processes.
- moq-tokio's `Connection` seeds a redial from the session it replaces, and
  the relay's cluster peers seed from the previous session to the same peer.
  The transport-upgrade quest's QUIC dial seeds from nothing, since the
  previous session ran over TCP.

Measure time to the encoder's target rate after a reconnect on the impaired
path profile, plus loss and latency during the jump. Ship it on by default
only when the jump never makes the first second worse than slow start.

## Required

- [Fork noq](/quest/next/quic/fork.md) - the controller wrapper lives there

## Related

- [Transport upgrade](/quest/next/transport-upgrade/README.md) - one of the
  reconnects this speeds up
- [Drain](/quest/next/drain/README.md) - GOAWAY redials are the other
