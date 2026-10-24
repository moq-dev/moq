# [M] FFI shutdown requests the drain synchronously

## Goal

`MoqSession::shutdown()` in moq-ffi is a plain (non-async) call that requests
the draining close before it returns, like `cancel(code)`. A caller waits for
the drain with the existing async `closed()`. Dropping or never polling a
future can no longer turn a shutdown into a cancel. Every binding and wrapper
follows, and the Impact bullet says how to migrate each affected package.

## Plan

Today `shutdown` is async and runs the close through `ffi::detached`
(`rs/moq-ffi/src/ffi.rs`), which aborts its task when the caller drops the
future. The async body only runs on first poll, so a caller that drops the
future early, as OBS's `MoQOutput::Stop` must avoid by holding it, gets a cancel
instead of a drain (found in [moq-dev/moq#6277](https://github.com/moq-dev/moq/pull/6277)).

Decided 2026-10-22 (maintainer): a synchronous `shutdown()` that only requests
the drain, waited on with `closed()`. Rejected: keeping it async and never
aborting the close (no API change, but the request still waits for a poll), and
an extra `begin_shutdown()` beside the async one (two methods for one action).

- Start the close on the FFI runtime before `shutdown()` returns, and report a
  close that fails through `closed()`.
- Follow the Cross-Package Sync row for `rs/moq-ffi`: `rs/moq-c` (or the
  generated C package, whichever ships first), the py, swift, kt, dart, and go
  wrappers, `cpp/moq`, `cpp/obs/src` (drop the "keep the future" workaround in
  `MoQOutput::Stop`), and `doc/lib/*`.
- Test: a shutdown whose result is never awaited still drains a finished track,
  with mocked time, in moq-ffi and through the OBS output test.
- The `release` line's additive async `drain()`
  ([FFI shutdown backport](/quest/m0/ffi-shutdown-backport.md)) maps onto this
  at the next breaking cut; say so in the Impact bullet.
- The `## Unreleased` entries in `rs/moq-ffi` and the wrapper guides say
  `shutdown` became async against release's synchronous one. The Impact
  bullet supersedes them, for the cut to collapse into the net change.

Public API: breaking in moq-ffi and every binding. Wire: none.

## Related

- [FFI shutdown backport](/quest/m0/ffi-shutdown-backport.md) - the release line's async `drain()`
