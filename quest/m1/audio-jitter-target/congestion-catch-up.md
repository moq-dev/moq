# [M] Audio recovers from a congestion stall quickly

## Goal

After a congestion stall, audio returns to the shared clock within a few
seconds instead of trailing by up to 1.4 s for 17 to 28 s, so the audio
quality harness's congestion rows no longer show a second of lip-sync error.
Congestion still costs a gap and a catch-up, as the
[epic's rule](/quest/m1/audio-jitter-target/README.md) says.

## Plan

Found by the triage of Nightly run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(2026-11-13). Caused by [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507):
the browser ring no longer trims audio that trails the clock. It keeps up to
the `stretch` bound (2 s by default, `js/watch/src/audio/latency.ts` `bound`)
and accelerates at about 48 ms/s. Chromium `opus-48000-congestion`
`av_lag_ms` p50 rose from 35 to 73 ms locally and p95 is 1434 ms against a
32 ms ceiling; trims fell from about 1000 ms/min to 0.

Decided 2026-11-13 (maintainer, /quest-plan):

- ✅ a regression to fix (rejected: accept it and let calibrate budget the
  congestion rows, which budgets the defect `calibrate.md` says to report).
- ✅ the implementer measures and picks (rejected: committing now to a lower
  default bound alone, or a faster catch-up alone). Try a lower default bound,
  past which audio skips onto the clock, and NetEq's fast accelerate for large
  lag (`rs/moq-audio/src/stretch.rs` already ports both operations). Pick by
  congestion `av_lag`, trims, and audible artifacts, and keep the browser and
  native players on one rule from one algorithm.

Put the before and after rows of `just test audio-quality` for chromium and
native in the PR.
