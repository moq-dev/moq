# [S] js/qmux drains a stream in linear time

## Goal

Reading a js/qmux stream costs time linear in the bytes and chunks received.
A peer that fills the 256 KiB stream window with one-byte STREAM frames no
longer stalls the browser: today draining it takes about 22 s in Chrome.
Every other js/qmux queue a peer can grow is linear too.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`): milestone ✅ m1 (rejected: m0, m3), audit score 7/10.

- `js/qmux/src/recv.ts:93` takes each chunk with `Array.shift()`, which is
  O(n) in V8, so draining is quadratic. Each STREAM frame pushes its own chunk
  (`recv.ts:166`). A Node repro took 2.75 s to drain 262,144 one-byte chunks;
  a head index took 0.3 ms. JSC (Bun) doesn't show it, so measure on V8.
- Use a head index or a small deque. The reset trimming (~`recv.ts:206-215`)
  and stop accounting (~`:241-243`) also walk the queue, so they must start
  at the head.
- Sweep the other `shift()` queues, `js/qmux/src/scheduler.ts:383`,
  `:410` and `js/qmux/src/session.ts:162`, `:338`. Fix each one whose
  length a peer controls, and share one queue type if more than one needs it.
- Test: one-byte chunks filling the window drain in bounded time on V8.

Public API: none. Wire: none.
