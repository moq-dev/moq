# [XS] A cancelled future reads as an error, not an abort

## Goal

`get()` after `cancel()` returns an error instead of aborting. Today the generated
`uniffi::Future` declares `void cancel() noexcept` on an lvalue, so
`future.cancel(); future.get();` compiles and then aborts at runtime, which
`cpp/moq/README.md` documents instead of preventing.

## Plan

- Mirror `std::future`, the idiom C++ callers know: a consumed future is
  invalid, `valid()` reports it, and reading it is an error (`std::future`
  throws `future_error(no_state)`). The package is expected-style with no
  exceptions, so after `cancel()` (and after `then()` or a move) `valid()`
  returns false and `get()` returns an error value instead of aborting.
  Decided with the maintainer after Codex on #4307 pointed out that
  `&&`-qualifying `cancel()` cannot stop `std::move(f).cancel(); f.get();`
  from compiling.
- Decided in [#4100](https://github.com/moq-dev/moq/pull/4100): cancel
  abandons the future, so no continuation runs. That still holds; only a
  later read of the dead handle changes, from an abort to an error.
- The `Future` template lives in the kixelated `uniffi-bindgen-cpp` fork, so
  this is a fork change and a new `-kixelated.N` tag, then the pin bump in
  `flake.nix` and every place its comment lists.
- Update the README sentence and every in-tree caller (`cpp/moq`, the probe,
  `cpp/obs`). Test the exact sequences: `cancel()` then `get()`, and
  `std::move(f).then(...)` then `f.get()`, each returning the error.

Public API: breaking on the unreleased C++ package only. Wire: none.
