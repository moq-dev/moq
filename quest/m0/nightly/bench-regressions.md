# [M] Paired bench regressions attributed

## Goal

Each regression the paired Rust comparison confirmed on 2026-10-27 is fixed at
its cause or shown to be noise by an A/A rerun, and Nightly's Rust bench
targets job passes.

## Plan

Nightly [40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673)
compared `22808bbb2f4aab9fdaf9a60507f77efbaef4bd0d` against
`8810d736518185eb813dc8758f8f95f31d50cc9e`. Its
[bench-rust artifact](https://github.com/moq-dev/moq/actions/runs/40506938673/artifacts/12739050694)
contains the estimates, confirmation rounds, and paired verdicts. The five
confirmed cases and their three confirmation deltas, in round order, are:

| Case | Round 1 | Round 2 | Round 3 | Median |
| --- | --- | --- | --- | --- |
| `retirement/selection/8x32` | +24.67% | +22.58% | +23.35% | +23.35% |
| `retirement/selection/32x32` | +13.31% | +20.86% | +22.55% | +20.86% |
| `retirement/selection/128x32` | +17.92% | +26.79% | +23.81% | +23.81% |
| `server_admission/workers=4/0` | +10.17% | +14.39% | +8.39% | +10.17% |
| `track_range_demand/1000r/1000` | +16.25% | +7.87% | +10.23% | +10.23% |

The original runner used aarch64 Linux with four allowed CPUs, rustc 1.98.1,
and LLVM 22.1.8. These are timing observations, not yet production attributions.
The newer [bench CI evidence](/quest/m1/bench-ci.md) supersedes the earlier
instruction to treat selection's agreeing sizes as proof: repeated rounds
reuse the same binaries, so layout or LTO changes can produce an agreeing
slowdown without extra production work. Keep thresholds and behavior intact.

Source inspection on 2026-10-29 narrowed the candidates, without measuring them:

- Admission's `Activity`, the admission benchmark, and all of `rs/kio` are
  unchanged across the original pair. Its four-worker contention requires
  paired timing and A/A controls; instruction counts alone cannot attribute
  scheduling or cache-line contention.
- [moq-dev/moq#6816](https://github.com/moq-dev/moq/pull/6816) added selection's
  answering lease so an unanswered epochless request is refused on replacement
  but survives a withdrawal. Count the replacement operation separately from
  fixture setup. Also account for `fuzz::RequestValidity::start` retaining only
  the current lease while production retains both leases: the benchmark can
  deallocate the answering channel during replacement instead of holding it
  until the request ends. This is a candidate measurement difference, not a
  demonstrated explanation for the ARM slowdown.
- Range-demand aggregation itself is unchanged.
  [moq-dev/moq#6815](https://github.com/moq-dev/moq/pull/6815) added the range
  refusal path to `Subscriber::update`. It reads the old preferences, constructs
  a consumer, and resolves/locks track state even for absolute group ranges,
  which need no cached anchor. Measure that path before deciding whether to
  move the wire-range check ahead of state lookup. Preserve unresolved-anchor
  refusal, covered by `draft22_resolves_cached_time_ends`.

Remaining work: reproduce these exact cases with separate target directories
per revision, three alternating paired rounds, same-binary A/A controls, and
instruction counts isolated to timed production operations. Attribute any
extra work before fixing it; equal counts alone do not prove timing noise.
Use the existing reader/range and route/consumer sweeps to verify scaling.
[moq-dev/moq#7253](https://github.com/moq-dev/moq/pull/7253) is an unfinished
checkpoint: both isolated builds waited at `quest gate` without reaching Cargo,
so it contains no new timings, instruction counts, or validated code fix.
Agents never dispatch Nightly. Bench-ci owns calibration; this quest owns the
five cases above.

Public API: none. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the threshold these crossed
