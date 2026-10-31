# [XS] Waiting: generated C async calls keep their failure contract

## Goal

Every async call in the generated C API has a callback contract a C caller can rely on for freeing `user_data`: the call returns a task, never NULL, and its callback runs exactly once, with the result or, when the task was freed first, with `cancelled` set.

[moq-dev/rust2#169](https://github.com/moq-dev/rust2/pull/169) implements it in rust2's C generator. Waiting on [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md) to move the C generator pin: `c/moq` adopts the new callback shape then, along with the CamelCase names, and `c/moq/test/probe.c`'s callbacks take `cancelled` and release `user_data` there. Delete this quest with that PR.

## Plan

From the 2026-10-22 security audit, as rust2#169 implements it. Decided with the maintainer on 2026-10-28:

1. Callback shape for a cancel:
   - ✅ `bool cancelled` right after `user_data` on every callback; with it set, `error` is NULL and `value` is empty
   - A generated status enum (completed or cancelled)
   - `cancelled` last
2. Where the cancelled callback runs:
   - ✅ Dispatched, on the dispatcher like a result (the host loop under `moq_set_dispatcher`), so it never runs inside `task_free` or nests inside a callback that frees another task
   - Inline in `task_free`, on the freeing thread
3. Panics:
   - ✅ A Rust panic aborts the process, as it does for every sync call under the expected style, so no callback is lost while the process lives
   - A panic error value in the expected style, which changes every sync call too

The rest of the contract:

- No generated async call fails before returning a task: arguments convert infallibly and misuse aborts, so the immediate-failure rule reduces to "returns a task, never NULL", stated once in the header's conventions and on each callback typedef.
- The result and the cancel share one atomic claim, so the callback runs once, and `task_free` no longer blocks on a callback running elsewhere. C++ mutexes do not poison. A dispatcher that refuses work or has shut down still abandons the call.

rust2's `c_dispatcher` and `c_api` tests cover freeing a pending, ready, or queued task, a callback freeing its own task or another's, and shutdown.

Public API: every generated async callback gains `bool cancelled`, and a cancelled task calls back once instead of never. Wire: none.
