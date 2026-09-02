# [S] The quinn backend's send-bandwidth estimate is cwnd/rtt, not a rate

## Goal

On the quinn backend, `estimated_send_rate()` reports the congestion
controller's bandwidth estimate, so the encoder rate policy and the lite PROBE
message run against a rate rather than a window divided by a latency. A
controller with no estimate (CUBIC) reports `None`, not a guess.

## Plan

`web-transport-quinn` returns `cwnd * 8 / rtt`. That only falls on loss or
RTT inflation, so under a loss-based controller it includes the standing
queue and a sender targeting a fraction of it maintains bufferbloat; it reads
stale-high when app-limited; and it rescales with RTT for reasons unrelated to
capacity. `moq_net::Session::send_bandwidth` samples it every 100 ms and feeds
`moq_video::encode::rate::Policy` and the lite PROBE, so the error propagates
across the session. quiche already reports its measured delivery rate.

quinn exposes the right number since
[quinn-rs/quinn#2802](https://github.com/quinn-rs/quinn/pull/2802) added
`PathStats::bandwidth_estimate` (BBR's estimate, `None` for CUBIC). It merged
after quinn-proto 0.11.17 shipped, so nothing published carries it yet, and
`web-transport-quinn` 0.12.1 still computes the window quotient. moq-native
defaults quinn to BBR, so the default path is populated once plumbed.

- In `moq-dev/web-transport`: return `path.bandwidth_estimate` from
  `estimated_send_rate()` with no cwnd/rtt fallback, and tighten the
  `web-transport-trait` doc to say the quantity is the controller's
  estimate, so a third backend does not invent a fourth interpretation.
  Release it.
- Here: bump quinn and web-transport-quinn, and check `moq_net`'s sampler
  behaves when the estimate flips between `Some` and `None` mid-session (it
  only creates the sampler when the first sample is `Some`).

## Required

- A quinn-proto release after 0.11.17 that carries quinn-rs/quinn#2802
- A web-transport-quinn release that returns `bandwidth_estimate` from `estimated_send_rate`

## Closes

- [#2847](https://github.com/moq-dev/moq/issues/2847) - close this issue when the quest finishes
