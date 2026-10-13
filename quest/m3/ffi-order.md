# [S] Group order in the bindings

## Goal

A native application picks the order a subscription's backlog arrives in,
oldest or newest first, through moq-ffi and every binding, the same way
`track::Subscription::order` does in Rust and `Subscription.order` does in JS.
Descending stays the default, so existing callers see no change.

## Plan

- No consumer asks for it yet. AGENTS.md keeps an API private until one does,
  so this waits here (#5385 left the FFI `MoqSubscription` without `order`).
- moq-ffi: a `MoqOrder` enum (`Ascending`, `Descending`) and an
  `order` field on `MoqSubscription`, defaulting to `Descending`, mapped in
  `From<MoqSubscription> for moq_net::track::Subscription`
  (`rs/moq-ffi/src/consumer.rs`).
- Follow the cross-package table in one PR: the wrappers in `py/moq-rs`,
  `swift`, `kt`, `dart/moq` and `go/wrapper/moq`, plus `cpp/moq`, and their
  `doc/lib/{py,swift,kt,go,dart,cpp}` pages. Each wrapper exposes it in its
  own idiom (an enum where the language has one).
- C: if [Generated C bindings](/quest/m1/c/README.md) has landed, C inherits
  it from moq-ffi. Otherwise add it to the hand-written `moq.h` and
  `doc/lib/c`.
- Test that an ascending subscription over a local broadcast receives a
  cached backlog oldest first, in moq-ffi and in every wrapper that has tests.

## Related

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - reshapes the published subscription surfaces, including moq-ffi; if it adds ranges to `MoqSubscription` first, order can ride along
- [Scope and sign track priority](/quest/m1/track-priority-scope.md) - changes `MoqSubscription.priority` to `i8` in every binding
