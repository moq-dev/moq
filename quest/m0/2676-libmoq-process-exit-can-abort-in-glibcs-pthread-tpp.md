# [M] libmoq: process exit can abort in glibc's __pthread_tpp_change_priority

## Goal

A C consumer that closes its session, waits for the terminal status, and
returns from `main` exits cleanly on Linux/glibc. The success path of
`test/smoke/clients/c/subscribe.c` returns instead of calling `_exit` to dodge
teardown.

## Plan

A program linking `libmoq.a` that consumed video can abort in atexit teardown
with glibc's priority-protected-mutex assertion, after the data path worked.
The smoke client works around it with `_exit(0)` the moment it has its frame
(17bf0d950, "fixes the flaky rust -> c leg"), with no diagnosis. `cpp/obs`
links `libmoq` the same way and follows the documented contract exactly, so
this is a real bug for embedders, not a flake.

What is known: `libmoq` runs a tokio current-thread runtime on a detached
`libmoq` thread behind a `LazyLock` (`rs/libmoq/src/ffi.rs`) that is never
shut down, and its global `State` is likewise never torn down, so worker
threads are live when C's atexit handlers and C++ static destructors run.
openh264 (vendored C++) is always linked. Nothing in our Rust creates a
priority-protected mutex, so either a `pthread_mutex_t` is used after free and
a garbage `__kind` routes an ordinary unlock down the TPP path, or a still
running thread touches state whose destructor already ran.

- Reproduce with a minimal C program (connect, consume video, close, wait for
  the terminal status, `return 0`) in a loop to get a hit rate. Take a
  backtrace from the core: `pthread_mutex_destroy`, `unlock`, or a static
  destructor narrows it a lot.
- Build `libmoq` under AddressSanitizer (the fuzz recipe in `rs/justfile`
  shows how to step off the pinned toolchain for `-Zsanitizer`) and run the
  loop; if a freed mutex is the cause ASan names it. Check whether it still
  reproduces with openh264 out of the link.
- Fix the mechanism where it lives. If it is teardown ordering, an atexit
  handler that quiesces the runtime thread before static destructors run is
  the likely shape; do not add a `moq_shutdown` the contract does not ask for.
- Remove `_exit` from the smoke client's success path. Returning is only safe
  once every registration that holds the stack `ctx_t` has fired its terminal
  callback: closing the session ends its status registration alone, while
  `moq_origin_consume_announced`, `moq_consume_catalog`, and
  `moq_consume_video` each keep the pointer until their own terminal, so close
  and drain all of them before `return`, or a late frame callback reproduces
  a second lifetime bug instead of isolating atexit teardown. The failure
  paths keep `_exit` for the `user_data` lifetime reason from #2675.

## Closes

- [#2676](https://github.com/moq-dev/moq/issues/2676) - close this issue when the quest finishes
