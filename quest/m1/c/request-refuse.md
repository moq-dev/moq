# [S] moq-c refuses only the waiting readers

## Goal

`moq_group_request_free` and `moq_group_request_abort` refuse the group to
the readers waiting on that request, as Rust's `Fill::refuse_groups` does,
instead of marking it dropped for every later reader.

## Plan

[moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) added
`Fill::refuse_groups` and moved Rust's fill paths onto it; moq-c's
`group_request_free` (`rs/moq-c/src/publish.rs:893`) still calls
`drop_groups`. The C signatures stay; only the behavior changes, so note it
in the PR's Impact. If the generated C API replaces the hand-written group
request calls first, delete this quest.

Public API: moq-c behavior change, no signature change. Wire: none.
