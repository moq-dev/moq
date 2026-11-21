# [XS] @moq/net rejects a received frame over 32 MiB

## Goal

Both JS wire readers (`js/net/src/lite/group.ts` and
`js/net/src/ietf/object.ts`) reject a plain frame whose declared size passes
`MAX_FRAME_BYTES` at its header with `FrameTooLarge`, before reading any of
it, as Rust's `content.rs` admission does. A local writer is unchanged.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the pre-cut flow-control
audit): ✅ additive, not a cut blocker (rejected: blocking the cut; dropping
it).

Today a received plain frame past one segment streams in at any size, and
`Consumer.readFrame()` (`#join` in `js/net/src/group.ts`) keeps every chunk
until it joins them, even after the group sheds them, and the browser
media path reads that way (`js/hang/src/container/consumer.ts`). Rust
relays already reject such frames, so only a browser connected directly to
a hostile peer is exposed. The 40 MiB tests in `group.test.ts` write
locally and stay.

Behavior change, no `!`. Public API: none. Wire: none.
