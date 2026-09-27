# [XS] A cancelled future cannot be read

## Goal

`get()` after `cancel()` no longer compiles. Today the generated
`uniffi::Future` declares `void cancel() noexcept` on an lvalue, so
`future.cancel(); future.get();` compiles and then aborts at runtime, which
`cpp/moq/README.md` documents instead of preventing.

## Plan

- Make `cancel()` `&&`-qualified, called as `std::move(future).cancel()`,
  the way `then()` already consumes the future. A moved-from future is the
  C++ idiom for "gone", so the misuse becomes unwritable in the common case
  and a use-after-move lint catches the rest.
- Decided in [#4100](https://github.com/moq-dev/moq/pull/4100): cancel
  abandons the future; it never delivers a cancelled error. Keep that.
- The `Future` template lives in the kixelated `uniffi-bindgen-cpp` fork, so
  this is a fork change and a new `-kixelated.N` tag, then the pin bump in
  `flake.nix` and every place its comment lists.
- Update the README sentence and every in-tree caller (`cpp/moq`, the probe,
  `cpp/obs`), and add a compile-fail check if the tests have a place for one.

Public API: breaking on the unreleased C++ package only. Wire: none.
