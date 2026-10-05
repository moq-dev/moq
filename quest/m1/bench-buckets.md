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
  whole run. Emit the interval's delta beside the cumulative fields, and
  derive [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md)'s
  per-interval percentiles from the same delta, so the two quests share one
  representation; whichever lands second builds on the other.
- 60,001 dense buckets per line is too much JSON. Pick a compact layout (sparse
  non-zero buckets, or a log-linear layout with bounded relative error) and
  record why.
- The output documents: the layout version, units, the interval's start and
  end, the sample count, and what an empty or partial interval (the first and
  the last) looks like.
- Publisher and subscriber stay on one host, so latency closes on one clock;
  `latency_clock_skew` keeps flagging a violation.
- A test sums fixture buckets from two runs and checks the percentiles
  against the concatenated samples, binned by the same layout and the same
  overflow bucket (60,000 ms and above today) before the percentile is taken. Update `rs/moq-bench/README.md`.

moq-bench is 0.0.x, so this lands on main.

Public API: none (CLI output only). Wire: none.

## Related

- [#3126](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - per-interval percentiles from the same buckets
- [moq.pro: benchmark output pin](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/kick/bench-pin.md) - the load harness that sums these across generators
