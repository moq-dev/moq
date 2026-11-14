# [S] fMP4 import at 1-byte chunks is no slower than before

## Goal

The interleaved fMP4 import fed in 1-byte chunks is back to its earlier cost,
or the slowdown is accepted with a recorded reason. Whole-input and 1 KiB
chunk imports are unaffected today.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7628](https://github.com/moq-dev/moq/pull/7628)'s
measurement: at 1-byte chunks `main` got about 19% slower (283 to 341 ms)
between [951cca678c](https://github.com/moq-dev/moq/commit/951cca678c3d02dd4671c41a5443567785832585)
and [96244dd854](https://github.com/moq-dev/moq/commit/96244dd854e4eeffedfdeaa9ef4a736266eb11b9),
separately from #7628. Suspects:
[moq-dev/moq#7491](https://github.com/moq-dev/moq/pull/7491),
[moq-dev/moq#7398](https://github.com/moq-dev/moq/pull/7398),
[moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985) and
[moq-dev/moq#7218](https://github.com/moq-dev/moq/pull/7218). Bisect on a quiet
host through `quest gate`, in paired, order-rotated rounds.
