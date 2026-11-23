# [S] moq-ffi's failing-first-dial test hangs in the full gate

## Goal

`moq-ffi`'s `test::session_state_names_a_failing_first_dial` passes every
run of `just gate`, fixed at its cause.

## Plan

Nightly's Gate job on 2026-11-22
([run 45035580806](https://github.com/moq-dev/moq/actions/runs/45035580806))
failed only on this test, which hit nextest's 120s timeout while 13,237
others passed. It drives its own paused runtime clock
(`crate::ffi::start_test_clock`) against a silent UDP socket, so a hang
points at a wait the manual clock never wakes, such as the dial's first
packet or `state_changed`, not at load.

High-risk PRs now run `just gate` in the merge queue, so this hang can drop
them from the queue as well as fail Nightly.

Reproduce with `cargo nextest run -p moq-ffi --lib
session_state_names_a_failing_first_dial --stress-count 200` under a
loaded host before changing it.
