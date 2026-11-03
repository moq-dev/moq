# [XS] moq-c's gated callback test frees nothing early

## Goal

`rs/moq-c/src/test.rs`'s `gated_callback` never touches `user_data` after the
test may have dropped it, so the moq-c tests stop aborting under load with
`corrupted size vs. prev_size` (SIGABRT). The fix is at the cause, never a
retry or a sleep.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from the SIGABRT in
[moq-dev/moq#7461](https://github.com/moq-dev/moq/pull/7461)'s Check run
(`moq-c test::follow_delivers_the_instance_again_after_an_end`).

- Cause: `gated_callback` sends `code` and only then locks `gate.hold`.
  Cancelling the follow is asynchronous, so after the terminal `0` arrives the
  test drops `gate` while the callback thread may still reach the freed
  `Box<Gate>`. The API guarantees `user_data` only until the terminal callback
  (`rs/moq-c/src/api.rs`, around line 2384). Introduced by
  [moq-dev/moq#5886](https://github.com/moq-dev/moq/pull/5886).
- Fix: take `gate.hold` before `gate.tx.send(code)`, so the test cannot free
  the gate while the callback still holds it. It is the only test callback that
  touches `user_data` after sending.
- Reproduce first: a temporary sleep between the send and the lock makes
  valgrind report the invalid write; keep a regression that fails without the
  fix if one is practical.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
