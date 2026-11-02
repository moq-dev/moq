# [M] E2EE in moq-ffi

## Goal

The Swift and Kotlin bindings publish and consume moq-e2ee protected
broadcasts under an application-supplied `(context, kid, secret)` credential,
interoperating with `@moq/e2ee` in both directions, with the same epoch rules
and typed authentication failures as Rust and JS.

## Plan

Decided 2026-10-28 with the maintainer (moq.pro T3 planning interview): this
is the quest the [encrypted consumer](/quest/m2/e2ee/consumer.md) defers FFI
to, adding both halves. The named consumer is moq.pro's
[T3 Code over MoQ](https://github.com/moq-dev/moq.pro/blob/main/quest/m3/t3/README.md)
mobile app, which receives its credential in a pairing link.

Wrap `moq-e2ee`'s producer and consumer entry points without new crypto in
the binding layer, and keep the encrypting publisher's refusal of a shared or
explicit epoch. Add a binding test that decrypts a JS-published broadcast and
the reverse to the interop suite.

## Required

- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - the producer these bindings wrap
- [Encrypted consumer shape](/quest/m2/e2ee/consumer.md) - the consumer these bindings wrap
