# [S] The moq-cli identities test passes every run over ietf22

## Goal

`moq-cli::integration identities::ietf22` (`rs/moq-cli/tests/integration/identities.rs`) passes
every run, alone and under load, fixed at the cause. Today it fails intermittently, panicking
with `called Result::unwrap() on an Err value: Old` at `identities.rs:64:34`, the reader's
`read_frame` in `read`. The `lite06` and `lite07` rows of the same test pass.

## Plan

Seen on macOS on 2026-11-28: 4 of 6 runs failed on origin/main `e1dd5f6b5b`, and 2 of 6 on
[moq-dev/moq#9823](https://github.com/moq-dev/moq/pull/9823), which doesn't touch the test's path.
Reproduce with
`just rs test -p moq-cli --test integration -E 'test(identities::ietf22)'`, repeated.

`Old` means the reader's group was dropped as older than what it should still read. Find whether the
ietf22 path hands the reader a group the relay already superseded, such as the first group of a
republished identity racing the subscription. Then decide whether the test expects more than
moq-transport promises, or the compatibility layer drops a group it owes, and fix that.
