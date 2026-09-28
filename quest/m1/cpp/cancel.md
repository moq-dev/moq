# [XS] A cancelled future says so before it is read

## Goal

Callers are told to check `valid()` before reading a future that may be
cancelled, consumed, or moved from, and the tests prove it goes false in each
case.

## Plan

The fork half is done on the line: the `-kixelated.2` generator's
`uniffi::Future` has `valid()`, false once `get()`, `then()`, `cancel()`, or a
move took its state, like `std::future`, and a read of an invalid future aborts
with "it was consumed or cancelled". `cpp/moq/test/probe.cpp` checks `valid()`
after `cancel()`. Decided in the #4307 review; `&&`-qualifying `cancel()` and an
error-returning `get()` stay rejected.

What remains:

- `cpp/moq/README.md` (the Error and Cancellation sections) still only says a
  misused future aborts; point callers at `valid()` instead.
- Extend the probe to check `valid()` after `then()` and after a move.
- Check the in-tree callers that can hold a cancelled future (`cpp/moq`,
  `cpp/obs`) against `valid()`.

Public API: none beyond the unreleased C++ package's `valid()`. Wire: none.
