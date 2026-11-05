# [M] A collapsing uplink lowers the bandwidth estimate promptly

## Goal

When a path's capacity collapses, the bandwidth estimate a sender reports
(the grant simulcast rungs follow) falls as soon as BBR3's own loss response
cuts, instead of holding the old rate for seconds. A before-and-after
measurement with the sparse-sender shaper harness shows the drop.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7671](https://github.com/moq-dev/moq/pull/7671)): ✅ the
reported estimate also honors BBR3's loss-driven bound; m1, beside the native
rung-disable proof. Not chosen: m2, or an investigation-only quest.

Evidence from #7671's sparse harness: after the path fell from 2 Mbit/s to
100 kbit/s, the estimate held 1.32 Mbit/s for about 11 s, and in the FFI run
the top rung disabled 5.6 s after the 500 kbit/s step. The probe idles while
the estimate covers the target, and the reported estimate
(`max_bw.min(bw_delay)`) ignores the loss bound BBR3 keeps
(`inflight_longterm` and its short-term bounds in
`rs/moq-congestion/src/bbr3/mod.rs`). Measure first, change second, and
record both runs. Keep the change inside the reported estimate unless the
measurement shows the control loop itself is late.
