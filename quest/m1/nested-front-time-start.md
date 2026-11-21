# [M] A front served by another front places a time start through it

## Goal

A reader of a front whose route is another front places a range's time start
as a reader of a plain route copy does, so its bounded read
ends once that range is read, in Rust (`rs/moq-net/src/model/resume.rs`) and JS
(`js/net/src/resume.ts`) alike.

## Plan

Found on [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278)'s review,
and reproduced on `main` at `b4496b1207`: in
`a_raised_start_behind_an_inner_front_hands_out_nothing_past_its_end`, a
range `7..=7` moved under the resume point of a replacement behind an inner
front never places (`resolved_range(0)` stays `None`), so the read stays
pending after every group lands.

Two gaps add up:

- Rust `Subscriber::resolved_ranges` answers `None` for every range of a
  front's reader (`Inner::Resume`), so the outer reader's `window` never
  learns where an inner front placed a range. JS already forwards it through
  `ResumeSource.resolved`; forward it in Rust from the inner reader's window.
- A backfill asked through an inner front is let go before the group holding
  the time lands on it: Rust `Copy::backs` and JS `#retains` ask
  `may_land`/`hooks.mayLand`, which a front's reader answers `false` for, as
  it holds no producer. Forward it to the inner reader's copies: true
  while any copy it still retains may land the group, not only its serving
  copy, since a replaced copy may still deliver one the serving copy cannot.

Extend that test, and its JS mirror, to assert the range places at 7..7 and
the read ends.

Public API: none. Wire: none.
