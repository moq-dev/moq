# [M] BBR3 schedules ProbeRTT as draft-07 does

## Goal

BBR3 enters ProbeRTT when its 10 s `min_rtt` filter expires, as
[draft-07](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-07.html#section-5.3.4)
does, halving ProbeRTT's cost for a backlogged sender, while an
application-limited sender still skips it and still learns a base RTT that
rose.

## Plan

draft-07 drops the 5 s `ProbeRTTInterval` and the `probe_rtt_min_delay`,
`probe_rtt_min_stamp`, and `probe_rtt_expired` fields, adds
`BBR.probe_rtt_cwnd` (half the BDP from the old `min_rtt`) to
`ProbeRTTCwnd()`, calls `SaveCwnd()` before `EnterProbeRTT()` (ours saves
after, so `prior_cwnd` only ratchets up), and replaces `ack_phase` with
`is_bw_probe_stopping` and `is_bw_probe_sample` with `check_probe_loss`.
Adopt all of it, citing -07 anchors throughout `rs/moq-congestion/src/bbr3/`.
QUICHE BBR3 also uses a 10 s period.

The application-limited ProbeRTT skip stays. Decided 2026-11-14 (agent pick
after the maintainer answered "I don't know", for review), keeping the
skip's benefit for media:

- ✅ Spurious-loss undo clears `probe_rtt_skip_congested`, since a loss
  proven spurious was not congestion.
- ✅ A skipped expiry adopts the expired window's lowest RTT sample as
  `min_rtt` instead of only refreshing its stamp, so a base RTT that rose
  is learned without running ProbeRTT; an application-limited sender's
  samples carry little self-induced queue.
- Rejected: QUICHE BBRv1's kBBR7 guard (run ProbeRTT when RTT drifted past
  12.5%), which would put ProbeRTT's half-BDP cap back on media; and denying
  the skip only on loss above the 2% threshold, which needs measurement.

The cut-short probe advance on ProbeRTT entry relies on the removed
PROBE_STARTING and PROBE_FEEDBACK phases; express it with
`check_probe_loss || is_bw_probe_stopping`. Test the 10 s schedule, the
save order, a base-RTT rise while application-limited, and a spurious loss
during a skip.
