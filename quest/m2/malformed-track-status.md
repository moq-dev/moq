# [S] A malformed track says so downstream

## Goal

When a relay aborts a track as malformed, such as a timed object without a
timestamp, every downstream subscriber learns that the track was malformed:
a specific status on moq-transport and lite, a distinct error in moq-net and
JS, and a distinct `MoqError` in the bindings. Today it ends as
`INTERNAL_ERROR` (0x0), which the bindings report as `MoqError.Internal`.

## Plan

Found 2026-10-28 while fixing [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135):
the relay aborted a JS publisher's track as malformed and every viewer saw
`Internal`, which hid the cause.

Decided 2026-10-28 (maintainer): m2, a diagnostics improvement rather than a
correctness bug.

- Check draft 22 (and 18) for an existing status or error code that fits a
  malformed track before adding one. If none exists, decide whether lite-07
  and our IETF profile need a new code, and record the decision.
- Carry it through moq-net's error type, js/net, moq-ffi, moq-c and the
  wrappers, so an app can tell a malformed upstream apart from a relay bug.

Public API: likely a new error variant in each binding. Wire: possibly a new
status code.
