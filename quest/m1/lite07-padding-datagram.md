# [S] Lite-07 padding datagram

## Goal

lite-07 defines a padding datagram that its receivers discard. With it, a
lite-07 sender can probe with unreliable, lowest-priority padding instead of
padding streams, and the receiver neither drops the session nor routes the
padding to a subscription. The draft's DATAGRAM section and lite-07
changelog define it. Rust (`rs/moq-net/src/lite/datagram.rs`) and JS
(`js/net/src/lite/datagram.ts`) decode and discard it on lite-07, with tests.
Nothing sends padding datagrams yet; the browser sender is
[Padding probes from the browser](/quest/m2/probe/padding.md).

## Plan

Decided 2026-10-30 (maintainer, on
[moq-dev/moq#7390](https://github.com/moq-dev/moq/pull/7390)): padding
datagrams are supported only on lite-07. Older lite versions and IETF
moq-transport do not get them, so no compatibility path is needed and only
lite-07 receivers handle the new type. The type must land before the
[lite-07 cut](/quest/m1/lite07-finalize.md), which is why this quest gates
it.

Today a lite datagram body is `Subscribe ID | Group Sequence | [Timestamp] |
[Inflated Size] | Payload` with no type, so a padding datagram needs a
discriminator. The options:

- A leading `Type (i)` on every lite-07 datagram: `0x0` group, `0x1` padding
  followed by arbitrary bytes. This mirrors the uni stream types, where
  Padding Stream is 0x2. Recommended.
- A reserved Subscribe ID that every receiver drops. This already works by
  accident, since unknown IDs are dropped silently. But it overloads the
  routing field, and a receiver can't tell padding from a stale ID.

Settle the choice when implementing, together with
[Lite-07 by start](/quest/m1/time-addressing/lite07.md), which also rewrites
the datagram body. Padding datagrams keep the 1200-byte limit. Like padding
streams, receiving one changes no session or subscription state.

## Related

- [Padding probes from the browser](/quest/m2/probe/padding.md) - the lite-07 sender that uses this type
