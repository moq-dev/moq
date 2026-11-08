# [S] js/qmux close

## Goal

A js/qmux session's close is one phase: open, closed cleanly, or aborted with
a cause. A peer's graceful APPLICATION_CLOSE ends the incoming stream and
datagram readables cleanly, the same as a local `close()` does. An abnormal
close, such as CONNECTION_CLOSE, a socket error, or an idle timeout, errors
them with its cause. Once the session is closed, nothing can change its
outcome.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

The bug (`js/qmux/src/session.ts`):

- On a received APPLICATION_CLOSE, the session sets
  `#closeReason ??= new Error(...)` and then calls the graceful `#close`.
- `#teardown` closes the incoming queues and datagrams with `#closeReason`.
  Its own comment relies on a graceful close leaving `#closeReason` unset.
- So a peer's graceful close errors every incoming readable.

`#closeReason ??=` is also written at several sites before `#abort`'s
`if (this.#closed) return`, so it can change after the session closed. And
`#sendClose` followed by the terminal call is a two-step protocol repeated
at about eight sites.

Recommended shape: `#state: { kind: "open" } | { kind: "closed", clean:
true, info } | { kind: "closed", clean: false, cause }`, with the close frame
carried in the transition.

Also sweep `js/qmux/src/credit.ts`, where `Credit`'s `#closed` and
`#closeReason` are always set together. Streams already have explicit states
(`SendHalf` in `send.ts`, `RecvStream` in `recv.ts`, RFC 9000 §3), so the
session close phase sits above them: its teardown ends every stream's halves.

[moq-dev/moq#8136](https://github.com/moq-dev/moq/pull/8136) and
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088) also edit
`session.ts`. Expect merge conflicts.
