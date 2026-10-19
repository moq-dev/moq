# [M] Dead code and API surface audit

## Goal

Code, exports, Cargo features, and CLI flags that nothing uses are deleted, or
are planned for deletion when removal breaks a public API.

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

Removing a public item is a breaking change and the maintainer's call, so it is
always a follow-up with its `MIGRATE.md` entry, never an inline fix.

Starting points (2026-10-19): `rs/` has 368 `allow(dead_code` and 40
`allow(unused`; justify each or delete what it hides. There is no
`[workspace.lints]`, and `unreachable_pub` is set only in moq-quic and
moq-sock. `cargo shear` catches unused dependencies, but nothing finds
`pub` items or JS exports with no consumer; try `knip` for `js/`. Check
Cargo features nobody enables and CLI flags no doc or test uses.
