# [S] Rust resume never releases a moved group

## Goal

Letting go of a replaced copy in Rust resume never releases a group already
moved to another copy, so that group never ends cleanly with frames missing.
A test proves it either way.

## Plan

[moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947) fixed this in
`@moq/net`, found by its model test. Rust's sweeps pass, but nobody checked
whether Rust has the same release path. Write the fixed case from that JS
fix against Rust; if it fails, fix it at the cause.

Public API: none.
