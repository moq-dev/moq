# [S] The shared pacer holds its rate under 2 Mbit/s

## Goal

Under 2 Mbit/s, a rate-paced controller on moq-quic (BBR3 today, and any candidate that sets a pacing rate) sends at its pacing rate, not up to twice it. BBR3 is retuned in the same PR so the nightly probe scenarios, `competing_cubic` included, stay within their late-frame and overhead budgets.

## Plan

Decided 2026-11-01 (maintainer): fix pacing overshoot and retune BBR3 together, without loosening any probe budget. This ships with BBR3 as moq-quic's default, so sending up to twice the rate below 2 Mbit/s is a release correctness issue.

Implementation is checkpointed in [moq-dev/moq#7606](https://github.com/moq-dev/moq/pull/7606): the pacer repays overshoot before earning new credit, and BBR3's minimum send quantum is one datagram instead of two. Deterministic unit sweeps cover low rates and two-packet batches. All 20 probe tests pass, including the full nightly seed sets. Over 256 paired competing-CUBIC seeds, probing costs +1.79 ± 0.58 late frames (standard error) against the +4.5 budget, and +0.009 ± 0.026 ms p99 against +40 ms. Every scenario's before/after late frames, p99 and overhead is recorded in the PR.

Remaining: complete the full local gate and mark the reviewed PR ready. `just ci` is blocked by mbx E2BIG ([moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594)); focused passes do not satisfy that gate. Do not bypass mbx or weaken assertions. The existing runner namespace denial remains another known full-gate limit.

The GCC, BBR3 delay-bound, and SCReAMv2 candidates ([moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546), [moq-dev/moq#7544](https://github.com/moq-dev/moq/pull/7544), [moq-dev/moq#7547](https://github.com/moq-dev/moq/pull/7547)) were all unmerged at measurement time, so no landed candidate rows applied. Recheck their state when resuming; rerun applicable rows if they have landed and record the shift in the [epic](/quest/m2/quic-realtime/README.md).

Public API: none. Wire: none.

## Related

- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - every candidate paces through this pacer
- [Probe up to a target bitrate](/quest/m2/probe/README.md) - its scenario budgets are what the retune must keep
