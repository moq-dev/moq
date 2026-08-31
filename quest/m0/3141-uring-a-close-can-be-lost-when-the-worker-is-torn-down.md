# [M] uring: a close can be lost when the worker is torn down before its send completes

## Goal

Implement and verify the behavior tracked in [#3141](https://github.com/moq-dev/moq/issues/3141)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Surfaced by the adversarial review of #3131. It applies to both QUIC backends, so it is filed separately rather than folded into that PR.

`udp::TxBuf::send` is fire-and-forget: it stages an SQE, and `Shared::push` only calls `ring.submit()` when the submission queue is full. So a packet handed to the socket has not necessarily reached the kernel, let alone the wire, until the worker enters the ring again.

The connection drivers rely on that ordering when they publish a terminal error for a close the application asked for: the driver flushes the CONNECTION\_CLOSE and then reports the close, so a caller waiting on `poll_closed` learns about it only after the packet is staged. If that caller then returns from `Worker::block_on` and drops the worker, the ring goes with it and the peer never hears the close, waiting out its idle timeout instead.

The window is narrow and the relay does not hit it (its workers outlive their sessions by design), but a one-shot client or a test that closes and immediately stops the runtime does.

Fixing it properly means a completion-aware send: something like `TxBuf::send` returning a handle the caller can await, or the socket tracking outstanding sends so a driver can wait for the one carrying its close. That is a `udp::Socket` API change, which is why it is not in #3131.

Related shape: the same property means any final flush (not just a close) can be lost on an immediate teardown, so a fix here should be about the socket's contract rather than the close path specifically.

## Closes

- [#3141](https://github.com/moq-dev/moq/issues/3141) - close this issue when the quest finishes

## Related

- [#3173: moq-uring: Worker drop submits an uncancelled receive and stalls for…](/quest/m0/3173-moq-uring-worker-drop-submits-an-uncancelled-receive-and.md) - related open work
