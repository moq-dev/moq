# Nightly passes on main

## Goal

The scheduled Nightly on `main` passes end to end, so a red run means a new
break instead of noise. Nightly is the backstop for what the PR gate skips on
purpose (benches, examples, feature shapes; see
[moq-dev/moq#5531](https://github.com/moq-dev/moq/pull/5531)), and it has
failed every night since at least 2026-10-19, which let a bench and two
example breaks reach `main` unnoticed.

## Plan

Decided 2026-10-24 (maintainer), from a triage of run
[40090959108](https://github.com/moq-dev/moq/actions/runs/40090959108): one
child per distinct cause, each landing on its own. The PR gate keeps
skipping benches and examples; a green Nightly catches them within a day.

This README's own work, after the children:

- Nightly's multi-step jobs (Features first) keep going past a failed step
  and fail at the end, so one break never hides the steps after it.
- Confirm a scheduled run passes. Failures already owned elsewhere stay
  there: the audio near-zero p95 row belongs to
  [audio jitter target](/quest/m1/audio-jitter-target/README.md), the
  macOS `receive_credit` timeout to
  [test flakes](/quest/m0/test-flakes-2/README.md), and the paired bench
  threshold to [bench CI](/quest/m1/bench-ci.md).

Public API: none. Wire: none.

## Required

- [The software renderer tests both capability paths](/quest/m0/nightly/render-capabilities.md) - the software-renderer lane verifies output selection with and without DMA-BUF import support without requiring hardware the runner lacks
- [The browser backend baseline completes on IETF22](/quest/m0/nightly/browser-baseline.md) - the WASM and UniFFI backend baseline completes every supported transport case without an internal error
- [Resolve the paired catalog fanout regressions](/quest/m0/nightly/paired-shared.md) - the reported catalog fanout regressions are attributed and genuine extra costs are fixed with paired measurements
- [Probing beside Cubic](/quest/m0/nightly/competing-cubic.md) - the `competing_cubic` probe test stays within its deadline budget
- [Gate stops hanging](/quest/m0/nightly/gate-hang.md) - `just gate` finishes instead of hanging to the timeout, and a hang names its module
- [moq-quic rustdoc](/quest/m0/nightly/quic-rustdoc.md) - `--all-features` rustdoc for moq-quic builds and a lane checks it
