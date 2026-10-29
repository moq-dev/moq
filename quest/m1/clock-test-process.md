# [XS] moq-clock's tests pass under cargo test

## Goal

`cargo test -p moq-clock` passes, not only under nextest.
`paused_runtimes_own_their_anchors` checks the same behavior without
depending on being the first test in its process to read the clock.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7161](https://github.com/moq-dev/moq/pull/7161): keep proving
that a paused runtime counting from its own origin anchors on its own clock.

[moq-dev/moq#7243](https://github.com/moq-dev/moq/pull/7243) isolates the test
with the existing `test-env` helper and asserts that both runtimes use their own
origins. Default and single-threaded cargo tests, nextest, and the crate's lint
checks pass. Bypassing per-runtime anchor selection still makes the test fail.

Remaining: validate the full `just ci` gate on a capable host, then remove this
quest and its link and mark the PR ready. The current host's full gate fails
because the mbx Cargo launcher reports `Argument list too long` and the Nix VM
image check cannot write `/proc/self/uid_map`. Keep those environment fixes out
of this test-only change.

Public API: none. Wire: none.
