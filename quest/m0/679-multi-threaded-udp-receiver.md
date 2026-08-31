# [S] Multi-threaded UDP Receiver

## Goal

Implement and verify the behavior tracked in [#679](https://github.com/moq-dev/moq/issues/679)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Quinn has a quite annoying limitation. It uses a single thread to receive all UDP packets.

This means `moq-relay` won't scale to multiple threads without significant packet loss. The same applies to a client if it's downloading/uploading a ton of media.

There's a few potential approaches:

1. Fix [quinn](https://docs.rs/quinn/latest/quinn/) (they want it)
2. Try using tokio-quiche (seems to support multi-sockets?)

I started on the 2nd approach with [web-transport-quiche](https://github.com/kixelated/web-transport/tree/main/web-transport-quiche) but I haven't tested it. It has some annoying limitations that might require rewriting `tokio-quiche`...

## Closes

- [#679](https://github.com/moq-dev/moq/issues/679) - close this issue when the quest finishes
