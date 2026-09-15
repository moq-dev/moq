# [XS] A malformed text catalog section fails the catalog in JS too

## Goal

`@moq/hang` treats the catalog `text` section the way Rust and its own
`json` and `binary` sections do: absent or not an object defaults to empty,
and a section that is an object with a malformed rendition refuses the whole
catalog. Today `js/hang/src/catalog/root.ts` wraps `text` in a blanket
`z.catch`, so a captions rendition with a bad field silently vanishes in the
browser while `rs/hang/src/catalog/mod.rs` `deserialize_section` rejects the
same catalog natively. The defect is on dev, where the text section lives.

## Plan

Route `text` through the same narrow fallback as `json` and `binary` in
`js/hang/src/catalog/section.ts`, and fix the comment above it: the
compatibility case it cites (an unrelated pre-existing `text` key) is still
covered when that key is not an object, and an object that is not a text
section was never a working catalog on the Rust side. Add a test on each
side of the line: an absent key parses to empty, a malformed rendition
rejects. Also correct the stale "subscription latency budget" message in
`js/net/src/group.ts`, which names the retired `Latency` type.

Public API: none. Wire: none.
