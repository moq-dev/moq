# [S] Subscriptions name the oldest group without a u64::MAX sentinel

## Goal

`Anchor` has an explicit variant for "the oldest group", and `Relative(n)` always means n groups before the latest. Today `Range::oldest` is `Anchor::Relative(u64::MAX)` (`rs/moq-net/src/model/subscription.rs`), against the repository's rule never to use a huge value as a marker. A peer on lite-07 or IETF can send `u64::MAX` or `u64::MAX - 1`, which clamp to the same group but differ in window start, demand shape and `is_wire`, and the lite encoder treats any `back > MAX_QUIC` as oldest, which the model does not.

## Plan

Found by the 2026-11-07 invariant audit (model-4). Add the variant, decode and encode it consistently on every wire (lite-07, lite-06 and earlier, IETF), and mirror it in JS and the bindings. m2 because `Anchor` is public API.

## Related

- [Subscribe ranges](/quest/m2/subscribe-ranges/README.md) - owns the range model this anchor lives in
