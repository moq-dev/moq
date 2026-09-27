# [XS] A cancelled future says so before it is read

## Goal

A caller can tell a cancelled or consumed future is dead before reading it,
and a read of one fails with a message naming the misuse. Today the generated
`uniffi::Future` declares `void cancel() noexcept` on an lvalue, so
`future.cancel(); future.get();` compiles and aborts with nothing to check
first; `cpp/moq/README.md` documents the abort instead of giving callers a
guard.

## Plan

- Mirror `std::future`, the idiom C++ callers know: once cancelled, consumed
  by `then()`, or moved from, a future is invalid and `valid()` returns false.
  Calling `get()` on an invalid future is a precondition violation, as it is
  for `std::future`, and it aborts with a message naming `cancel`/`then`/move
  rather than a bare abort. Decided with the maintainer during the #4307
  review.
- Rejected: `&&`-qualifying `cancel()`, since `std::move(f).cancel(); f.get();`
  still compiles; and `get()` returning an error, since expected mode has no
  error value for it: generic `E` has no invalid-state variant, and the line
  README already rejects `std::variant<E, Cancelled>`.
- Decided in [#4100](https://github.com/moq-dev/moq/pull/4100): cancel
  abandons the future, so no continuation runs. That still holds.
- The `Future` template lives in the kixelated `uniffi-bindgen-cpp` fork, so
  this is a fork change and a new `-kixelated.N` tag, then the pin bump in
  `flake.nix` and every place its comment lists.
- Update the README to point callers at `valid()`, and check it in the
  in-tree callers that can hold a cancelled future (`cpp/moq`, the probe,
  `cpp/obs`). Test `valid()` after `cancel()`, after `then()`, and after a
  move.

Public API: additive `valid()` on the unreleased C++ package. Wire: none.
