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

Progress (2026-10-19): the JSON stream consumer reuses the byte stream
consumer's single-group lifecycle instead of maintaining its own receive,
roll detection, and teardown state machine. JSON validation, error identity,
and concurrent-read refusal stay at the JSON boundary. Its record count,
payload size, and compression sweep runs in the JavaScript nightly lane.

The initial scan covered private Rust traits, media/container dispatch,
transport adapters, JSON codecs, and duplicated JavaScript utilities.
Backend and playback traits have multiple implementations or device-free
test doubles; catalog traits erase application extension types. Keep those
seams. Transport adapter deduplication and compression removal already have
dedicated work; avoid a second design in this audit.

This is a checkpoint, not the finished run. Review the remaining large
publisher/subscriber modules and binding layers before completing it or
adding the successor. Public wrapper and shared-helper reshaping still need
maintainer proposals, rather than new exports solely to remove a few lines.
