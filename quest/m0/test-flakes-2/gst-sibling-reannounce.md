# [S] A reannounced gst sibling never replays its finish under load

## Goal

`moq-gst`'s `source::imp::session_tests::a_reannounced_sibling_instance_does_not_replay_its_finish`
passes reliably under a loaded `just check --all`, fixed at its cause, and is
re-enabled: its `#[ignore]` and its `sh/gh/test-inventory.allow` entry go.

## Plan

[moq-dev/moq#8438](https://github.com/moq-dev/moq/pull/8438) disabled it after
one failure in a loaded `just check`: the finished track replayed on its pad
(2 starts instead of 1). It passed 3/3 alone. No single PR is known to have
caused it; find out whether the test races real time or `moqsrc` really can
replay a finished track when the sibling's re-announce wins the race against
its end, and fix whichever it is. If it is a product bug, the PR is a `fix`.

Decided 2026-11-11 (maintainer, follow-up planning in /quest-complete): ✅
plan it as a child of this m0 epic (rejected: leave the test ignored).
