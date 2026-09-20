# [M] Measure transcode resource and startup costs

## Goal

The transcode ladder has measured thread, codec-session, memory, and startup
costs under live and fetched demand, and removes demonstrated overhead without
changing its public API or starving a rung behind another.

## Plan

Live rungs already share one decoder. The feed holds 16 items; each rung admits
four concurrent fetch pipelines. Those per-rung limits do not bound aggregate
GPU sessions or codec worker threads across broadcasts. Every newly resolved
rung also probes a temporary real encoder and synthetic frame before catalog
publication. macOS sinks execute inline; other hosts create codec threads.

Measure increasing broadcast/rung counts, live plus fetch demand, slow encoders,
source resize, and demand churn. Include executor responsiveness, retained GPU
frames, time to first catalog/frame, and cancellation cleanup. Preserve the
honest probed codec description rather than replacing probes with guesses.
Only change scheduling, pooling, or internal budgets after identifying the cost.

Keep group-boundary drain coverage independent of hardware: a delayed fake
decoder must deliver the tail inside its original live/fetched group. Existing
Consumer EOF tests and hardware-conditional VAAPI tests are not that same proof.
Wire resource/ownership regressions and reproducible benchmarks into CI or the
existing nightly harness. Public API and wire: unchanged.

## Related

- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - repeatable measurements
- [Adaptive ladder](/quest/m2/ladder/README.md) - bandwidth control is separate from process resource cost
