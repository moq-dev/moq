# [M] Simplification audit

## Goal

The code and APIs that cost the most complexity for what they deliver are
simplified: duplicate mechanisms merged, layers, options, and generics with
one user collapsed, and features that don't earn their keep narrowed or
removed. The result is net simpler, even where that means less scope.

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

Narrowing a feature or reshaping an API is the maintainer's call, so those
are proposals with a recommendation, filed as follow-ups. Unlike the [dead
code audit](/quest/m0/audit-dead-code.md), this targets code that is used but
overbuilt.

Starting points (2026-10-19): look for one concept implemented twice (across
crates, or Rust and JS drifting apart), traits and generics with a single
implementation, config knobs every caller sets the same way, wrappers that
only forward, and the largest modules and functions. Prefer reusing what the
stack already has over a parallel mechanism.
