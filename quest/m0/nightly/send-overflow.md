# [S] moq-net compiles in Fuzz and send-depth

## Goal

Nightly's six Fuzz jobs and Tests (rs send-depth) compile `moq-net` again.

## Plan

Nightly [40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303)
(2026-10-25, at 66af72315b) fails both with "overflow evaluating the
requirement ... Send" on the async block at
`rs/moq-net/src/ietf/publisher/tracks.rs:826` (`run_publish` into
`resume_publish`). The PR gate does not build these configurations, so the
nesting grew unnoticed. Reproduce with the send-depth recipe, then make the
future shallower at its source (box or split the nested publish future)
rather than raising the recursion limit.

Public API: none. Wire: none.
