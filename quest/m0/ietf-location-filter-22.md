# [S] Draft-22 LOCATION_FILTER carries its type

## Goal

On moqt-22, LOCATION_FILTER (0x21) is encoded and decoded the way draft-22
section 9.20.9 defines it, in both `moq-net` and `@moq/net`: no Length, a
Location Filter Type varint, then only the fields that type names. A draft-22
peer reads our default Next Object as Next Object, not as an absolute start
at `{0, 0}`. Drafts 20 and 21 keep the length-inferred form.

## Plan

Verified 2026-10-06 against draft-ietf-moq-transport-22: the changelog lists
this as the draft's only wire change (moq-wg/moq-transport#1913, #1953). Types:
0x00 none, 0x01 relative StartGroup, 0x02 absolute start, 0x03 adds
EndGroupDelta, 0x04 adds EndObject, 0x05 Next Object. Any other type is a
PROTOCOL_VIOLATION.

Decided 2026-10-06:

- m0, ahead of Seattle interop on 2026-10-12: today a draft-22 peer silently
  reads the whole track, and every other form mis-frames the parameters
  after it.
- Rust and JS in one PR, since both share the bug and only talk to each
  other in `test/interop`.
- Gate on a separate draft-22 check. `is_draft20` / `isDraft20` also gate
  fill, INCLUDE_PROPERTIES and joins, which did not change.

Where:

- Rust: `rs/moq-net/src/ietf/filter.rs`, `Param for Filter` and the
  FILL_PARAMETERS scope in `Param for Fill`. Every 0x21 use goes through
  `Param for Filter`.
- JS: `js/net/src/ietf/parameters.ts` frames 0x21 as length-prefixed bytes
  because its id is odd, stores them raw, and `filter.decode` parses them
  later. On draft-22 the decoder must read the type and its fields straight
  from the stream, so the stored value changes too; `decodeInline` in
  `filter.ts` is the pattern. This is the opposite of the Range Filters'
  exception, which adds a Length to even ids. `encodeFill` hard-codes
  `encodeLengthPrefixed` for 0x21, and `FILL_ALLOWED` maps it as `"bytes"`.
- The spec still says a "zero-length" LOCATION_FILTER inside FILL_PARAMETERS
  means the whole track (section 3.4). On draft-22 that is type 0x00.

Tests: byte vectors from the draft-22 figure for every type in both
languages, including `21 05` for Next Object and an unknown type refused.
Update the tests that pin the length-prefixed bytes on Draft22 (Rust
`fetch.rs`, which decodes `21 03 04 00 02` for drafts 20 to 22 in one loop
that Draft22 must leave, and `subscribe.rs`; JS `ietf.test.ts`,
`filter.test.ts`).
`test/interop` cannot catch this, since every client shares the codec.

Public API: none. Wire: moqt-22 LOCATION_FILTER changes to the draft's form.

## Closes

- [#4847](https://github.com/moq-dev/moq/issues/4847) - draft-22 LOCATION_FILTER should drop the Length and carry the Location Filter Type
