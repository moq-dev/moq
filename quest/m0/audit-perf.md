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
that history and against the numbers this Plan records. `sh/gh/nightly.sh`
skips the moq-e2ee, moq-json, moq-archive and moq-cli benches, mux `jitter`,
and 5 of 6 moq-uring benches. Fan-out paths need a sweep over both axes
(AGENTS.md). JS perf is judged in a browser, not node or bun. PR-time gating
belongs to [benchmark regressions in CI](/quest/m2/bench-ci.md); don't
duplicate it.
