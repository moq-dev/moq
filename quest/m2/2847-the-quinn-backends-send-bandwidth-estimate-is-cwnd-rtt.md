# [XS] The quinn backend's send-bandwidth estimate is cwnd/rtt, not a rate

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

- Bump quinn and web-transport-quinn to the releases that carry the estimate.
- Check `moq_net`'s sampler behaves when the estimate flips between `Some`
  and `None` mid-session: it only creates the sampler when the first sample is
  `Some`, and a controller swap or an app-limited stretch can change that.

## Required

- [Bandwidth estimate release](/quest/m2/web-transport-bandwidth-estimate.md) - the upstream change and the release this bumps to

## Closes

- [#2847](https://github.com/moq-dev/moq/issues/2847) - close this issue when the quest finishes
