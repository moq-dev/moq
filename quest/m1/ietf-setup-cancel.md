# [M] SUBSCRIBE setup stops when the subscriber cancels

## Goal

A subscriber that cancels a SUBSCRIBE while the publisher is still setting it
up releases the upstream demand and the request slot at once, on every
moq-transport draft, in Rust and JS.

## Plan

In `rs/moq-net/src/ietf/publisher.rs`, a SUBSCRIBE's setup
(`request_broadcast`, `subscribe_track`, and the `poll_live` wait) races only
the auth gate, not the subscriber withdrawing it (UNSUBSCRIBE, or a FIN or
reset on its request stream). Until the producer answers, a cancelled
request keeps upstream demand and its slot. Resume uses the same setup.
Raised on [moq-dev/moq#6299](https://github.com/moq-dev/moq/pull/6299), where
the maintainer placed it in SUBSCRIBE setup as a whole.

Race setup against withdrawal, mirror it in `@moq/net`, and test a cancel
mid-setup with a producer that never answers.

Public API: none. Wire: none.
