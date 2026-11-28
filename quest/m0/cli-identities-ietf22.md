# [S] moq-cli's ietf22 identity replacement test passes on macOS

## Goal

`moq-cli::integration identities::ietf22` (`rs/moq-cli/tests/integration/identities.rs`) passes
every run on macOS, fixed at its cause. Today it fails every run on an Apple Silicon host at
`origin/main` `5a7973d818`: `read` panics at the `group.read_frame().await.unwrap()` that
checks a group holds one finished frame, with `Err(Old)`. The merge queue's Linux run passes,
so the queue does not catch it.

## Plan

Reproduce with `cargo nextest run -p moq-cli --test integration identities::ietf22`, find
which `read` call in `replacement` gets `Old` (the backtrace names only `read`), and decide
whether the reader or the test is wrong. [moq-dev/moq#9807](https://github.com/moq-dev/moq/pull/9807)
last changed this test; check whether the failure starts there.

Public API: none. Wire: none.

## Hosts

- macos - it fails only there so far
