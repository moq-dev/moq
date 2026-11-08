# [M] E2EE in moq-ffi

## Goal

The Swift and Kotlin bindings publish and consume moq-crypto protected
broadcasts under an application-supplied `(context, kid, secret)` credential,
interoperating with `@moq/crypto` in both directions, with the same epoch rules
and typed authentication failures as Rust and JS.

## Plan

Decided 2026-10-28 with the maintainer (moq.pro T3 planning interview): this
is the quest the [encrypted consumer](/quest/m5/e2ee/consumer.md) defers FFI
to, adding both halves. The named consumer is moq.pro's
[T3 Code over MoQ](https://github.com/moq-dev/moq.pro/blob/main/quest/m3/t3/README.md)
mobile app, which receives its credential in a pairing link.

Wrap `moq-crypto`'s producer and consumer entry points without new crypto in
the binding layer, and keep the encrypting publisher's refusal of a shared or
explicit epoch. Add a binding test that decrypts a JS-published broadcast and
the reverse to the interop suite.

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): the bindings wrap moq-crypto's concrete `broadcast::{Producer, Consumer}` (`rs/moq-crypto/src/e2ee/broadcast.rs`, already public) directly, without waiting on [net-wrap](/quest/m5/net-wrap/README.md). So this quest requires neither e2ee quest below, both of which wait on net-wrap: it exposes encrypted tracks and frames, which is what the T3 app reads. Encrypted media publishing through moq-mux's catalog and codec publishers comes with [Rust protected publisher seams](/quest/m5/e2ee/rust-publish.md), and the bindings pick it up then without a break.

## Related

- [Rust protected publisher seams](/quest/m5/e2ee/rust-publish.md) - moq-mux publishing into an encrypted broadcast, after net-wrap
- [Encrypted consumer shape](/quest/m5/e2ee/consumer.md) - Rust and JS media readers over an encrypted broadcast, after net-wrap
