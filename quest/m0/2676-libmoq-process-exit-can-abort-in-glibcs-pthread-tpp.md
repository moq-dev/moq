# [M] libmoq: process exit can abort in glibc's __pthread_tpp_change_priority

## Goal

Implement and verify the behavior tracked in [#2676](https://github.com/moq-dev/moq/issues/2676)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Symptom

A C program that links `libmoq.a`, consumes a video track, and then exits normally (returning from `main`, or calling `exit`) can abort during atexit teardown with glibc's priority-protected-mutex assertion in `__pthread_tpp_change_priority`. The process dies with SIGABRT (exit 134) and a core dump *after* the MoQ data path has already worked correctly.

This has only been observed on Linux/glibc.

#### Where it bit us

The C leg of the interop smoke test (`test/smoke/clients/c/subscribe.c`). 17bf0d950 worked around it by having the client `_exit(0)` the moment it has its frame, skipping atexit teardown entirely, with the commit message "Fixes the flaky rust -> c leg". The workaround was never accompanied by a diagnosis, so per CLAUDE.md's Root Cause First this should be treated as a real bug rather than a flake that's been made to go away.

moq-dev/moq#2675 extends the same `_exit` to the client's failure paths, for a separate reason (a use-after-free on `user_data`). It does not touch this.

#### Why this matters beyond the smoke test

The smoke client is not the only C consumer. `cpp/obs` links `libmoq` the same way, and the C API's whole contract is "call `moq_session_close`, wait for the terminal `on_status`, exit". A consumer that follows that contract exactly should not abort. Right now the only thing keeping our own test green is that it skips the C runtime's exit path, which is not advice we can reasonably give an embedder.

#### What is known

- `libmoq.a` statically bundles `moq-video`, whose software H.264 fallback is openh264 (vendored C++; `stdc++` is in `rs/libmoq/native-libs/linux.txt` for exactly that reason).
- Nothing in our Rust code creates a priority-protected mutex. `grep` for `PTHREAD_PRIO`/`pthread_mutexattr`/`SCHED_` across `rs/moq-video` and `rs/libmoq` comes back empty, so the mutex in question comes from a dependency or is not really a PRIO\_PROTECT mutex at all (see below).
- libmoq runs its own tokio runtime on a `LazyLock` thread (`rs/libmoq/src/state.rs`), which is never shut down; its threads are still live when C's atexit handlers and static destructors run.

#### Hypotheses, none confirmed

1. **A `pthread_mutex_t` is used after free during teardown.** `__pthread_tpp_change_priority` is only reached when `m->__data.__kind` says PRIO\_PROTECT. If the mutex memory has already been freed, a garbage `__kind` can route an ordinary unlock/destroy down the TPP path with a nonsense priority, which is precisely what that assertion catches. This would make the assertion a *symptom* of a lifetime bug, not of anything priority-related.
2. **A race between the still-running tokio/openh264 worker threads and C's static destructors.** We never stop the runtime, so a worker can touch state whose destructor already ran.
3. **Something in the static-link arrangement**, e.g. openh264's C++ statics being destroyed while a thread is inside them.

Hypothesis 1 is the one worth checking first, because it is the only one that would also be a live bug for a well-behaved embedder rather than a teardown-ordering nuisance.

#### Suggested investigation

- Reproduce with a minimal C program: connect, consume video, `moq_session_close`, wait for the terminal `on_status`, `return 0`. Loop it to get a hit rate.
- Run it under ASan and under valgrind/helgrind. If hypothesis 1 is right, ASan should name the freed mutex directly.
- Get a backtrace from the core: whether the abort comes from `pthread_mutex_destroy`, `pthread_mutex_unlock`, or a static destructor narrows this a lot.
- Check whether it still reproduces with `moq-video`'s software H.264 fallback out of the link, which would implicate openh264 specifically.

#### Definition of done

A C consumer that returns from `main` after closing its session exits cleanly, and `test/smoke/clients/c/subscribe.c` no longer needs `_exit` to dodge teardown.

## Closes

- [#2676](https://github.com/moq-dev/moq/issues/2676) - close this issue when the quest finishes
