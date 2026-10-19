# [M] Performance audit

## Goal

Every benchmark that regressed since the previous run is explained, fixed, or
planned, and every hot or fan-out path without a benchmark gets one or a
follow-up.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Starting points (2026-10-19): there are 54 `[[bench]]` targets across 14 crates
(counted from `rs/*/Cargo.toml`) and 27 JS bench files. Nightly stores their history with
`github-action-benchmark` (`bench/costs`, `bench/rates`), but both
steps set `fail-on-alert: false`, so regressions go unseen. Compare against
that history and against the numbers this Plan records. The audit found
`sh/gh/nightly.sh` skipped 13 targets: moq-e2ee, moq-json, moq-archive and
moq-cli benches, mux `jitter`, and 5 of 6 moq-uring benches. The current fix
schedules all 54 targets and checks the manifest inventory in the existing
nightly tooling tests. Supplemental and io_uring measurements have separate
one-hour jobs. Handwritten allocation, window, and playback-buffer tables
remain in the raw run logs; only Criterion estimates enter the chart report.
Fan-out paths need a sweep over both axes
(AGENTS.md). JS perf is judged in a browser, not node or bun. PR-time gating
belongs to [benchmark regressions in CI](/quest/m2/bench-ci.md); don't
duplicate it.

Audit evidence (2026-10-19, incomplete):

- There is no `gh-pages` branch yet, so `bench/costs` and `bench/rates`
  cannot supply the previous-run comparison. The October 18 Nightly
  ([39124936449](https://github.com/moq-dev/moq/actions/runs/39124936449))
  retains `bench-rust` with 1,527 measured series at `50da59e67`.
  The October 19 Nightly
  ([39266265648](https://github.com/moq-dev/moq/actions/runs/39266265648))
  reports a successful Rust bench job but retains no `bench-rust` artifact.
  It took 39m50s, so new coverage must not extend that leg.
  The JS job was cancelled at one hour after the interests benchmark stopped
  printing output; the test-cost audit is investigating that root cause.
- Both nights retain binding reports. Single-run signals needing paired
  reproduction include Kotlin 1 KiB, 4 publishers x 4 subscribers CPU:
  79.08 to 122.73 ms/MB (+55.2%); Dart small-frame throughput/CPU changes
  around 12-14%; and Go 1 KiB, 1 publisher x 4 subscribers throughput:
  207.96 to 181.35 MB/s (-12.8%). There are 31 report entries with a cost
  increase above 10%, including ratios duplicating the same underlying
  measurements. These are candidates, not attributed code regressions.
  Inspect the Kotlin Gradle dependency update `2c11728f9` when reproducing;
  its presence between runs does not establish causation.
- Browser artifacts are correctness smoke runs: one round, no warmup,
  Chromium 153.0.8010.12 versus 156.0.8078.4. They cannot establish a JS
  performance regression. Run the browser harness in paired full mode
  with the same browser before drawing conclusions.
- Local verification: `just check`, the six nightly/inventory tests, six
  benchmark-report tests, and `quest check` passed. Supplemental compilation
  completed in 10m58s, including the full CLI binary's fat LTO, but the
  optional fixture run was stopped during jitter under shared-host load.
  It did not complete or produce a validated report. The new io_uring group
  was not run locally. Revalidate both groups in Nightly before relying on
  their measurements.
- Still required before completing this run: restore comparable Rust/JS
  history, reproduce the binding candidates, audit unbenchmarked hot and
  fan-out paths, and record the resulting baselines. Keep this quest in
  m0 until those checks finish; add its m1 successor only then.
