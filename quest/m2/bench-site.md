# [S] Benchmark trend site

## Goal

A public site charts nightly benchmarks over time at
<https://git.moq.dev/bench/>, so a regression or an optimization shows as a
step in the trend. It carries the [binding benchmarks](/bench/README.md) and
the nightly Rust Criterion and JS benchmarks, and other sources can join
without a new site.

## Plan

The pipeline landed in #5759 (`9fcb3f327`) and #6045: the Nightly workflow
keeps history on `gh-pages` and deploys github-action-benchmark's charts.
Pages is on (GitHub Actions source, custom domain `git.moq.dev`), and
<http://git.moq.dev/bench/> serves the charts (checked 2026-10-20).

Left:

- HTTPS: <https://git.moq.dev/bench/> fails, and Pages reports HTTPS not
  enforced. Get the certificate provisioned for `git.moq.dev`, then enforce
  HTTPS. Changing the Pages setting is a
  repository setting: ask the maintainer or get approval for the `gh api`
  call.
- The report root <https://git.moq.dev/> returns GitHub's 404, though
  `bench/README.md` says the nightly reports index links the trend site.
  Deploy that index.
- The "Rust benchmark tests" job is not charted: its nextest benchmarks
  print no structured measurements.

Decided with the maintainer on 2026-10-15:

- **GitHub Pages via github-action-benchmark**; its own alerting stays off,
  so tracking is informational. Regression alerts are
  [bench-ci](/quest/m2/bench-ci.md)'s paired comparison.
- **One branch for everything**: bench-ci publishes into the same report
  rather than its own data branch.
- Hosted runners are noisy across runs; chart the binding / Rust ratios
  next to the raw numbers, since those hold steady across runners.
- The relay load benches (`bench-audio`, `bench-profile`) and the
  audio-quality job's benchmarks can join later through the same input
  format.

Public API: none. Wire: none.
