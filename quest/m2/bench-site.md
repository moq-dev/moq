# [M] Benchmark trend site

## Goal

A public GitHub Pages site charts nightly benchmarks over time, so a
regression or an optimization shows as a step in the trend. It carries the
[binding benchmarks](/bench/README.md) and the existing nightly Rust
Criterion and JS benchmarks, and other sources can join without a new
site.

## Plan

Decided with the maintainer on 2026-10-15:

- **GitHub Pages via github-action-benchmark**: the nightly pushes each
  run's results to a `gh-pages` branch, and the action's built-in charts are
  the site. Its own alerting stays off; tracking is informational.
- **Sources**: the binding benchmark's JSON and the "Rust benchmarks" and
  "JS benchmarks" nightly jobs, converted to the action's input format where
  they don't already emit one. Several are smoke runs today: most Rust
  Criterion targets run with `-- --test`, which measures nothing. Switch the
  charted targets to a measuring mode (or add a measurement pass), weighing
  the added nightly runtime, and check each JS bench reports a measurement
  before charting it. Nightly size history (today a 45-day
  artifact) is a natural extra series if it fits cheaply. The relay load
  benches (`bench-audio`, `bench-lock`) and the audio-quality job's
  benchmarks are out of scope here; they can join later through the same
  input format.
- **One branch for everything**: [bench-ci](/quest/m2/bench-ci.md) records
  its nightly Criterion history here instead of creating its own data
  branch.
- Hosted runners are noisy across runs; chart the binding / Rust ratios
  next to the raw numbers, since those hold steady across runners.
- Enabling Pages is a repository setting: ask the maintainer to switch it
  on (or approve the `gh api` call) at that step.
- Document the site URL, what each series means, and local reproduction in
  `bench/README.md`.

Public API: none. Wire: none.
