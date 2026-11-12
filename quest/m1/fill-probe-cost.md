# [M] A JS fill probe costs what changed, not every unknown run

## Goal

A JS reader's fill probe (`#askFills` in `js/net/src/track.ts`) costs the runs that changed since its last probe, as the Rust `Misses::judge` does, rather than calling `#askMiss` on every received stale run it does not trust and every hole below the edge on each probe.
A publisher fragmenting stale drops then costs each reader linear work over the flood, not quadratic or worse.

## Plan

- Found on [moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476); `main` has the same probe shape: with N unsettled stale runs above the received edge, each probe walks all N, and each `#askMiss` scans `#asked`, `#refilled`, and the fills linearly. A bench of one probe per new drop took 0.35 s at N = 250, 2.5 s at 500, and 19 s at 1000.
- Recommended shape: judge each sequence once, as the Rust `Misses` keeps `judged`, and judge again only what an eviction, a refusal, a widening, or a moved window reopened.
- Keep the groups asked the same as today: the JS memo asks fills above the live edge per run, which the Rust grace deferral handles differently.
- Measure with a bench swept over the run count and the readers, as fan-out work requires.
