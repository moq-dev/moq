# [M] JS frames

## Goal

The js/net frame path (`js/net/bench/frames.ts`) is no slower than
`release` when measured in a browser, or the gap is accepted with a reason
recorded in the [epic](/quest/m1/release-perf/README.md).

## Plan

[PR #6296](https://github.com/moq-dev/moq/pull/6296) removed an accidental
retained-history scan on accepted untimed tracks. Unknown metadata still checks
buffered timestamps, and timed and wall-clock expiry keep their behavior. A
retained-groups x publishers x readers benchmark runs in Chromium and nightly.

Measured in headless Chromium 156.0.8078.4, with one full warmup per persistent
page, three alternating rounds, identical current/A/A bundles, and unchanged
`frames.ts` on release and main. Small-chunk IETF medians improve 32 to 41%:
16 B frames / 24 B chunks 4295 to 2860 ns/frame (release 2545), and 1000 B /
1008 B 6060 to 4140 ns/frame (release 3835). A/A ratios for those rows are
0.99 and 1.04. The retained-history sweep checks each live payload, excludes
replay/setup, and parks reads before writing. With 1024 retained groups, all
publisher/reader shapes improve in every round; medians fall from 15.6 to
63.6 us/frame to 1.9 to 2.9 us/frame. Small fixtures remain noisy.

The original Signal.race prototype had no consistent browser benefit and was
dropped. Profiles show per-write idle activity clock reads and expiry work on
main that release lacks, but that does not establish every remaining gap as
intended cost. Lite and some batched IETF rows remain above release, with noisy
A/A controls. Do not call parity complete or accept those gaps implicitly.

Decided 2026-10-22: land the scoped scan fix and its browser sweep in
[PR #6296](https://github.com/moq-dev/moq/pull/6296), without accepting the
remaining gap, which is unattributed and noisy. This quest stays open for:

- Attribute the remaining stable release gaps, then fix accidental cost or ask
  the maintainer to accept the intended feature cost with a reason in the epic.
- Re-run the browser comparison after the other release-perf children land.
