# [M] Dependency and alternatives audit

## Goal

Dependencies are current, deduplicated, needed, and the best maintained option,
and hand-rolled non-core code that a maintained library covers is planned for
replacement.

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

Research the state of the art for each candidate: compare maintenance,
performance, and API fit before planning a swap.

Starting points (2026-10-19): Dependabot bumps weekly with a 7-day cooldown, so
look for the majors it cannot take. `deny.toml` warns on duplicate versions;
count them in `Cargo.lock` and `bun.lock`. `cargo deny` runs only
nightly, although the `deny.toml` header says `just rs check` runs it.
Nothing audits JS advisories. Look for unmaintained dependencies (RUSTSEC
`unmaintained`, no release in a year) and for hand-rolled utilities a
maintained crate or package now covers.
