# [XS] Generated C async calls state and keep their failure contract

## Goal

Every async call in the generated C API has a callback contract a C caller can rely on for freeing `user_data`:

- A call that fails before returning a task returns no task and never invokes its callback, so the caller still owns `user_data`.
- A call that returned a task invokes its callback exactly once with a terminal result, even when the work panics or a lock it shares is poisoned.

Today the hand-written `moq_session_connect` says to call close even on error, and its bandwidth wait calls `callback.call` outside `ffi::run`, so a panic there drops the terminal callback silently. The generated API replaces that ABI, so the contract belongs in the generator's async plumbing rather than in `rs/moq-c`.

## Plan

From the 2026-10-22 security audit:

- Document the immediate-failure rule on each async callback typedef in the generated header, once in the generator rather than per function.
- Run the terminal callback through the same panic and poisoning handling as the rest of the call, so cleanup cannot be lost.

Tests: a call refused before it starts fires no callback; a call whose work panics still delivers one terminal callback.

Public API: header documentation and a guaranteed terminal callback. Wire: none.

## Required

- [moq-c package](/quest/m1/c/package.md) - the generated C API this contract applies to
