# [S] A cancelled MoqTrackDemand wait never loses the next change

## Goal

Cancelling a `MoqTrackDemand::wanted_changed` call, from any binding, never
consumes a demand change: the next call returns that change instead of
waiting for the one after it. `MoqFill::wanted_changed` already behaves this
way.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7036](https://github.com/moq-dev/moq/pull/7036).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- `MoqTrackDemand::wanted_changed` (`rs/moq-ffi/src/demand.rs:45`) runs the
  watch through `crate::ffi::detached`, a spawned task. A call cancelled
  before the binding frees it leaves that task running; it advances the
  shared watcher and its result is dropped, so the change is lost.
- `MoqFill::wanted_changed` (`rs/moq-ffi/src/producer.rs:519`) fixed the same
  race by driving the watch in place through `crate::ffi::Task::run` ("a
  cancelled call left unfreed must not advance the watcher"). Use the same
  shape for `MoqTrackDemand`, and check the other `detached` watchers in
  moq-ffi and moq-c's demand callbacks for the same pattern.
- Test: start a wait, cancel it, change demand, and assert the next call
  returns the change at once.

Public API: none. Wire: none.
