# [S] Benchmark 200 ms audio groups against 100 ms

## Goal

Measure whether 200 ms audio groups beat the 100 ms default on relay cost
without hurting quality or join latency, and bring the default back to the
maintainer only if they clearly do. Non-goal: changing the default here.

## Plan

Decided 2026-11-24 (maintainer): also benchmark 200 ms against 100 ms. Run
`bench-audio` and `just test audio-quality` at both spans, with at least two
runs per arm since single audio-quality runs sit within noise. Record the
numbers in the PR; if 200 ms wins clearly on cost without hurting quality or
join latency, bring the default back to the maintainer as a `Waiting`
decision rather than changing it here. For reference, #9561 measured a
32 x 8 room at 2.05 cores / 105 MiB with one group per packet and 0.85 cores /
61 MiB at 100 ms.

`bench-audio` already sweeps per-packet, 100 and 200 ms grouping. The
audio-quality arm needs a build with `moq_mux::container::Join::DEFAULT` at
200 ms, since its publishers group through the container importers.

The binding importers already group at `Join::DEFAULT` through
`moq_mux::import::Track` ([moq-dev/moq#9823](https://github.com/moq-dev/moq/pull/9823)).
