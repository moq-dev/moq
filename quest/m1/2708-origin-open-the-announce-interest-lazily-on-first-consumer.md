# [S] origin: open the announce interest lazily, on first consumer

## Goal

Implement and verify the behavior tracked in [#2708](https://github.com/moq-dev/moq/issues/2708)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

A session wired to feed an origin (JS: the `subscribe` option in #2705; Rust: `Client::with_subscriber`) opens an announce-interest stream with an empty prefix as soon as it attaches, whether or not anything ever reads an announcement. On a large relay that is announce traffic and per-session announce state for every broadcast the session may see, paid even by an app that only publishes or only consumes known paths via requests.

The origin knows whether anyone is interested: an open `announced()` stream, an announcement-gated watch handle, or (narrower) which prefixes they cover. The forwarder could open the wire announce stream only while interest exists, and scope it to the union of requested prefixes rather than the root.

Applies to both implementations; neither is lazy today. The JS forwarder lives in `js/net/src/connection/forward.ts`; the Rust equivalent is the subscriber wiring in `rs/moq-net` session setup.

## Closes

- [#2708](https://github.com/moq-dev/moq/issues/2708) - close this issue when the quest finishes
