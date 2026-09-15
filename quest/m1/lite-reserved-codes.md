# [S] Register the four placeholder stream codes

## Goal

Every stream code a moq-lite endpoint sends is assigned in the draft's own
48-63 range and decodes back to the same condition on the other side. Today
Rust sends `Unroutable` (0x21), `WrongSize` (0x24), `FrameTooLarge` (0x25),
and `TimestampMismatch` (0x26) from the reserved 32-47 range, which the draft
says a receiver must treat as unspecified, and `from_code` returns them as
`Unknown`. moq.pro adopts the release that follows the merge, so this is the
last free window to move them.

## Plan

Assign the next free lite codes after `EVICTED` (0x35) in
`drafts/draft-lcurley-moq-lite.md`, one row each with the same one-line
meaning the Rust variant carries, and drop the "provisional placeholders"
paragraph: 32-47 stays reserved with nothing emitted in it. Send and read the
new values in `rs/moq-net/src/error.rs` (`to_code`, `from_code`) and in
`js/net/src/error.ts` (`StreamCode`, the `FrameTooLarge` subclass), so a
condition raised on one side is the same named condition on the other. Extend
the round-trip test that currently asserts the four stay inside 0x20..0x30 to
assert they now round-trip, and the moq-transport bridge maps each to the
registered code with the same meaning or to an unspecified error, per the
draft's rule for this range. Validate with `just drafts check`.

Public API: no Rust or JS signature changes; the `StreamCode` constants change
value. Wire: four stream reset codes move from 0x21/0x24/0x25/0x26 to their
assigned values.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so nothing provisional ships on the released wire
