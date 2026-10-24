# [S] One nextest build in the queue

## Goal

`test rs` compiles once: the workspace, moq-video and moq-audio with capture
features, and moq-cli with `play,openh264` share one feature-unified build, and
the tests each runs today still run.

## Plan

Decided with the maintainer on 2026-10-23. Measured 2026-10-23: `test rs` is 44%
of queue module time, and compiling dominates it. It runs three separate
`cargo nextest` builds; the workspace build alone takes 1m44 to 4m24 against
33 to 51 s of test running, and moq-cli's `--bins` build takes up to 2m17 for a
1.3 s test run. One build is estimated at 1 to 2.5 min off Rust entries.

- Check that unifying features does not change what any test exercises.

Public API: none. Wire: none.
