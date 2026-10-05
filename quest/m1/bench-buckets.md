# [S] moq-bench: mergeable interval buckets

## Goal

Every `moq-bench` JSONL line carries that interval's latency histogram in a
versioned, documented bucket layout, so the buckets from several bench
processes on several hosts sum element-wise over any window into one
distribution. A percentile computed once from the summed buckets matches one
computed from the concatenated samples binned by the same layout and overflow
rule: the buckets are lossy, so the match is exact at bucket resolution, not
at raw-sample precision.

## Plan

Decided 2026-10-05 in moq.pro's quest audit: percentiles neither window nor
merge, and moq.pro's load harness sums several generators against one relay,
so the bench emits mergeable buckets. Approved as recommended.

- `Latency` (`rs/moq-bench/src/stats.rs`) already counts 1 ms buckets for the
  whole run. [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md)
  adds the interval's delta for its per-interval percentiles; this quest
  emits that same delta in a mergeable layout, so the two share one
  representation.
- 60,001 dense buckets per line is too much JSON. Pick a compact layout (sparse
  non-zero buckets, or a log-linear layout with bounded relative error) and
  record why.
- The output documents: the layout version, units, the interval's start and
  end, the sample count, and what an empty or partial interval (the first and
  the last) looks like.
- Harness rule: publisher and subscriber run on one host, so latency closes
  on one clock and buckets from several hosts merge without skew;
  `latency_clock_skew` keeps flagging a violation. Replace the README's
  advice to NTP-sync separate publisher and subscriber hosts
  (`rs/moq-bench/README.md:117-118`) with this rule.
- A test sums fixture buckets from two runs and checks the percentiles
  against the concatenated samples, binned by the same layout and the same
  overflow bucket (60,000 ms and above today) before the percentile is
  taken. Update `rs/moq-bench/README.md`.

Lands on main, then is backported to `release` as an additive cherry-pick
PR, like [Stats linger](/quest/m0/stats-linger.md), since moq.pro's load
harness tracks `release` (decided 2026-10-05). The backport carries #3126's
delta along if `release` does not have it yet.

Public API: none (CLI output only). Wire: none.

## Required

- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - the per-interval bucket delta these buckets emit

## Related

- [moq.pro: benchmark output pin](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/kick/bench-pin.md) - the load harness that sums these across generators
