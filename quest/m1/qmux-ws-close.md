# [S] js/qmux's WebSocket ponyfill ends its writable on close

## Goal

In js/qmux's WebSocketStream ponyfill, once the socket closes, whether the
peer closed it, it errored, or it was adopted already closed, writes to the
writable reject instead of resolving while the bytes are discarded. A normal
close also removes the AbortSignal listener, so a long-lived signal reused
across connections no longer keeps every closed socket alive.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`): milestone ✅ m1 (rejected: m3), audit scores 6/10 and 5/10.

- Writable (`js/qmux/src/web-socket-stream.ts:210-236`): it never keeps its
  controller, and `onclose` (`:259-265`), `onerror` (`:252-258`) and the
  already-closed adopt path (`:272-278`) only settle the readable and the
  `opened`/`closed` promises. A write after close calls `ws.send` on a dead
  socket, which per WHATWG discards the data and grows `bufferedAmount`, so
  writes succeed until `drain` (`:148-157`) passes the high-water mark. Error
  the writable from every terminal path. The audit reproduced this with a real
  WebSocket peer. Do the same in a test.
- Abort listener (`:284`): `{ once: true }` only removes it once an abort
  fires. Remove it on every terminal path, for example with a cleanup
  AbortController's signal in the listener options. The native path (`:337`)
  hands the signal to the platform and isn't affected.
- Model the socket's lifecycle as one state, as the
  [explicit-state epic](/quest/m1/explicit-state/README.md) asks, so each
  terminal path settles every half at once.

Public API: none. Wire: none.
