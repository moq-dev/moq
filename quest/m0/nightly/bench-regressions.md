# [M] Bisect the moq-audio bus and moq-mux parameter_sets regressions

## Goal

Two benchmark regressions Nightly flagged are explained and either fixed or
accepted with a recorded reason:

- 13 `moq-audio/bus` cases (`bus_minus`, `bus_mixes`, `bus_sources`), +16 to
  +29% together, between `fbef872` and `b5805859`.
- `moq-mux/parameter_sets h264-split-pps/distinct`, +14% / +26% / +46% at
  n = 16 / 256 / 4096, so the cost grows with n.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete from the triage of Nightly
runs [42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523)
(the bus set, in "Rust supplemental benchmarks") and
[42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545) (the
parameter sets, in "Rust shared")). Both look real: the bus cases moved
together, and the parameter_sets cost scales with n, which points at a
per-item cost that grew rather than noise.

Suspects from the triage, to confirm by bisecting with the paired
comparison (`bench/README.md#paired-comparison`), not by reading:
[moq-dev/moq#8121](https://github.com/moq-dev/moq/pull/8121) (encoders take a
join) and [moq-dev/moq#8010](https://github.com/moq-dev/moq/pull/8010)
(comfort noise) for the bus; [moq-dev/moq#8140](https://github.com/moq-dev/moq/pull/8140)
and [moq-dev/moq#8199](https://github.com/moq-dev/moq/pull/8199) for the
parameter sets. Name the culprit PR under `## Caused by`. A regression that
buys correctness may stay, with the trade recorded in the PR.

Public API: none expected. Wire: none.
