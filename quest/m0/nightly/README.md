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

Decided 2026-10-27 (maintainer), from the coverage audit's triage of run
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673): more
children for the causes nothing owned.

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

- [The browser backend baseline completes on IETF22](/quest/m0/nightly/browser-baseline.md) - the WASM and UniFFI backend baseline completes every supported transport case without an internal error
- [Probing beside Cubic](/quest/m0/nightly/competing-cubic.md) - the `competing_cubic` probe test stays within its deadline and p99 budgets
- [Paired bench regressions bisected](/quest/m0/nightly/bench-regressions.md) - retirement/selection's +21-24% and two 10% rows fixed or shown to be noise
- [Firefox's player closes every VideoFrame](/quest/m0/nightly/firefox-videoframe.md) - the Media job's Firefox player stops leaking an unclosed VideoFrame
- [moq-tokio's isolated feature builds pass](/quest/m0/nightly/tokio-features.md) - `Incoming::refuse` stops failing `tokio-features --all` on dead_code, so the rest of `rs features` runs
