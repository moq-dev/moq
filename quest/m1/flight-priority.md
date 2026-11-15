# [XS] A flight asks upstream with its riders' priority

## Goal

A flight's upstream fill (`track::board` in `rs/moq-net/src/model/track.rs`)
asks with the priority and delay of the reader that boarded it, as every other
fill does, so a late joiner's replay is not ranked below all live traffic on a
congested upstream link.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md):
`board` hardcodes `fill::Asking { priority: 0, max_delay: None }`. Pass the
rider's `Preferences` in, and add a test that the fill carries them.

Public API: none. Wire: none.
