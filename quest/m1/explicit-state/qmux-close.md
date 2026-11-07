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

Also sweep `js/qmux/src/recv.ts` and `js/qmux/src/credit.ts`:

- In `recv.ts`, `#fin`, `#error`, and `#terminal` encode the stream's end,
  and `error()` never marks it terminal.
- In `credit.ts`, `#closed` and `#closeReason` are always set together.

[moq-dev/moq#8136](https://github.com/moq-dev/moq/pull/8136) and
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088) also edit
`session.ts`. Expect merge conflicts.

## Related

- [JS qmux streams follow QUIC's per-stream state machine](/quest/m0/qmux-js-credit.md) - its stream states subsume the `recv.ts` end flags; sweep `recv.ts` on top of it
