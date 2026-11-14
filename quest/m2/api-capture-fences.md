# [XS] Capture listings and selections can grow

## Goal

moq-video's capture API takes new fields without a break:

- `Camera`, `Display`, `Window`, and `App`, the structs `cameras()`,
  `displays()`, `windows()`, and `apps()` return, are `#[non_exhaustive]`.
- `Source::Display` takes a `#[non_exhaustive]` selection struct in place of
  `Option<String>`, built by a constructor from the display id, so an
  exclusion list or a capture option joins it later.

## Plan

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ fence these
now (rejected: a second moq-video break after the cut; adding exclusion as a
separate setting outside `Source`). Two later quests add fields here:
[capture listing](/quest/m3/capture-list.md) (placement, kind, identity) and
[capture exclusion](/quest/m4/capture-exclude.md), which says the exclusion
belongs to the display selection. Neither is pulled into this cut; this quest
only keeps them additive.

Check the other `Source` variants while here: a tuple variant holding
`Option<String>` has the same problem wherever a later quest plans a second
field. Fence those the same way and list each in the PR. Mirror the selection
struct in moq-ffi and the bindings that expose `Source`.

Public API: breaking for moq-video callers that build or destructure these
(`Source::Display(None)` becomes the constructor), in Rust and the bindings.
Wire: none.

## Related

- [Capture listing](/quest/m3/capture-list.md) - adds fields to the listing structs
- [Capture exclusion](/quest/m4/capture-exclude.md) - adds the exclusion to the display selection
