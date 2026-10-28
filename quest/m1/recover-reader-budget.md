# [S] Recover judges a miss by the reader's own budget

## Goal

When resume decides whether a missing group is still coming, it judges by
the reader's own budget, not the copy's combined one, in Rust and `@moq/net`.
Right after a failover, a short-budget reader no longer settles a group for
an hour-budget reader sharing the copy.

## Plan

Found by [moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947)'s model
test, which never triggered it once routes answered fetches. Reproduce it
with a fixed case first, then extend the model test so a random sweep can
hit it.

Public API: none. Wire: none.
