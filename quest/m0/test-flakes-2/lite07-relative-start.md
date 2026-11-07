# [S] A lite-07 relative start resolves to the publisher's start every time

## Goal

A lite-07 subscription whose relative start counts back from a frameless
newest group always reports the publisher's start. `js/net/src/integration.test.ts`'s
"integration: lite-07 reports a relative start over a frameless newest group"
passes reliably under load, and its `test.skip` is removed.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8165](https://github.com/moq-dev/moq/pull/8165)'s validation:
in a baseline run on unchanged main, `reader.start()` returned `0n` where the
test expects the publisher's `5n`. It is quarantined with `test.skip` until
this quest fixes it, per AGENTS.md.

Reproduce it first, under load or with `bun test --rerun-each`, and find
which side reports 0: the copy resolving the start before the publisher's
answer arrives, or the publisher counting back from the frameless newest
group. Fix it at that cause, not in the test. Check the Rust mirror,
`a_relative_start_reports_the_publishers_start_over_a_frameless_newest_group`
(`rs/moq-net/tests/integration/range_anchors.rs`), for the same ordering.
Prove the fix with a test that forces the losing order, not with repeated
runs.

Public API: none. Wire: none.
