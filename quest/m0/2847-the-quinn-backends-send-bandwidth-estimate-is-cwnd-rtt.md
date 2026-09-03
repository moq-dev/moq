# [M] The quinn backend's send-bandwidth estimate is cwnd/rtt, not a rate

## Goal

Implement and verify the behavior tracked in [#2847](https://github.com/moq-dev/moq/issues/2847)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

`estimated_send_rate()` is documented in `web-transport-trait` as "estimated available send bandwidth", but the two backends return fundamentally different quantities:

- **quiche** (`web-transport-quiche`): `path.delivery_rate * 8`, i.e. the congestion controller's measured delivery rate. This is a rate.
- **quinn** (`web-transport-quinn`): `cwnd * 8 / rtt`.

```rust
fn estimated_send_rate(&self) -> Option<u64> {
    let rtt_secs = self.rtt.as_secs_f64();
    if self.stats.path.cwnd > 0 && rtt_secs > 0.0 {
        Some((self.stats.path.cwnd as f64 * 8.0 / rtt_secs) as u64)
    } else {
        None
    }
}
```

`cwnd / rtt` is not a bandwidth estimate. It's a window divided by a latency, which means:

- **It only falls on loss or RTT inflation.** Under a loss-based controller, steady-state `cwnd` includes the standing queue, so the number reported is the bottleneck rate *plus* whatever is sitting in the buffer. A sender targeting a fraction of it maintains bufferbloat rather than avoiding it.
- **It is stale-high when app-limited.** `cwnd` stops growing when you stop filling it, but it doesn't shrink either, so an idle-ish sender reads a ceiling from whenever the pipe was last full.
- **It moves with RTT for reasons unrelated to capacity.** A route change or a cross-traffic latency spike rescales the estimate without the available bandwidth having changed.

`moq_net::Session::send_bandwidth` samples this every 100ms and it is the input to `moq_video::encode::rate::Policy`, which is where it turns into a real encoder bitrate. It is also what the lite PROBE message reports to the peer (`lite/publisher.rs`), so a bad estimate propagates across the session rather than staying local.

#### What's needed

The BBR pacing rate. quinn already computes it and it is already exposed on a public, non-qlog-gated trait method:

```rust
// quinn_proto::congestion::Controller
fn metrics(&self) -> ControllerMetrics { .. }

pub struct ControllerMetrics {
    pub congestion_window: u64,
    pub ssthresh: Option<u64>,
    pub pacing_rate: Option<u64>,   // bits/s
}
```

`Bbr::metrics()` populates `pacing_rate: Some(self.pacing_rate * 8)`; `Cubic::metrics()` leaves it `None`, which is the correct signal for "this controller has no rate estimate". Today the only consumer is `qlog_recovery_metrics`, behind the `qlog` feature, so the value never reaches `ConnectionStats`.

`moq-native` already defaults quinn to BBR (`congestion_control(quic)` -> `CongestionControl::Delay` -> `BbrConfig`), so the pacing rate would be populated on the default path.

Fix chain, cheapest first:

1. **quinn**: add `pacing_rate: Option<u64>` to `PathStats`, populated from `congestion.metrics().pacing_rate` without the qlog gate. The value is already computed on every ack; this is plumbing it into the stats snapshot.
2. **web-transport-quinn**: return `path.pacing_rate` from `estimated_send_rate()`. Falling back to `cwnd / rtt` when it's `None` (CUBIC) is defensible, but returning `None` is more honest and lets a caller distinguish "no estimate" from "a bad one".
3. **web-transport-trait**: tighten the doc on `estimated_send_rate` to say what the quantity is, so a third backend doesn't invent a fourth interpretation.

#### Why now

[#2815](https://github.com/moq-dev/moq/issues/2815) divides this estimate among concurrent encoders sharing a connection. That makes the arithmetic honest, but every share inherits whatever error is in the input, so the allocator is only as good as this number. Worth fixing independently: today a single encoder on the quinn backend is already rate-controlling against a window, not a rate.

## Closes

- [#2847](https://github.com/moq-dev/moq/issues/2847) - close this issue when the quest finishes

