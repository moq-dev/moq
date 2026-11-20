# [XS] Miri skips the host's suspend clock

## Goal

`Tests (rs miri)` passes again: kio's
`clock::captured::tests::a_suspend_is_counted_apart_from_the_instant` no
longer asks Miri for a clock it does not support.

## Plan

Found by the 2026-11-19 triage of run [44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083):

```
clock 7 is unsupported
```

Clock 7 is `CLOCK_BOOTTIME`, which the test reads through the host clock's
`suspended()` (`rs/kio/src/clock/captured.rs:236`). Its neighbours in
`rs/kio/src/clock/runtime.rs` skip host reads with
`#[cfg_attr(miri, ignore = "...")]`. Recommended: split the host read into
its own test ignored under Miri, so the simulated half still runs there.

Caused by: [moq-dev/moq#9169](https://github.com/moq-dev/moq/pull/9169).

Public API: none. Wire: none.
