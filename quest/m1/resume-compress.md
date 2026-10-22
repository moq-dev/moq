# [XS] Rust resume refuses a route copy with different compression

## Goal

A same-epoch route whose track declares a different `compress` than the
logical track's first answer is refused before any of its groups are
forwarded, as JS already does. Today a compressed route A replaced by an
uncompressed route B under the same epoch keeps A's metadata while serving B's
groups, so a downstream lite-07 reader fails on the next group with a wrong
frame size.

## Plan

Found by an external audit on 2026-10-22, confirmed on main: `Front::track_info`
(`rs/moq-net/src/model/front.rs`) compares timescale, max age, and priority,
not `compress`. JS's `compatible()` (`js/net/src/resume.ts`) compares all four.

- Add `compress` to the comparison. A mismatch takes the existing refusal
  path, with the same classification as the other mismatched fields.
- `refresh` stays unchecked on both sides: it is a join heuristic, not
  framing.
- Regression test through mocked sessions in
  `rs/moq-net/tests/integration/route_change.rs`: compressed A serves a group,
  uncompressed B takes over under the same epoch, and the reader never sees
  B's groups decoded under A's metadata. Matching compression keeps resuming.

Public API: behavior only. Wire: none.

## Related

- [Rust resume holds a declared end](/quest/m1/resume-rust-end.md) - the other Rust copy-compatibility check
