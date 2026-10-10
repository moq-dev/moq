# [S] JSON window consumers return each event's timestamp

## Goal

The moq-json and `@moq/json` window consumers return each event with a
timestamp. In JS all three data consumers then return `Timed<T>`; the Rust
snapshot and stream consumers follow in
[Data consumer timestamps](/quest/m1/data-consumer-timestamps.md).

## Plan

Follow-up from #5099, which gave `@moq/json` and `@moq/flate` snapshot and
stream consumers `Timed<T>`. The window consumer (`window::Consumer::next` in
`rs/moq-json` and `js/json`) still yields a bare `Event<T>`.

- Decided (2026-10-10): `next()` returns `Timed<Event<T>>`. `at` is the
  timestamp of the frame that carried the event, and `None` when untimed. A
  record restated by a group header gets the header frame's time; document that
  on the type. There is no per-record time on the wire, so no wire change.
- Decided: Rust and JS together, same names and shape. Update moq-ffi and the
  bindings if they expose the window consumer (see the Cross-Package Sync table).
- Update in-repo readers (`js/hang` timeline, `js/room` chat) and add a line to
  the Unreleased section of `doc/setup/upgrade.md`. No new doc page.
- Land before the next `@moq/json` release, so its break ships together with
  #5099's. Not blocked on the Rust snapshot and stream quest: both Rust breaks
  use the same `moq_net::Timed` and ship in the next moq-json release together.

Public API: breaking (Rust and JS). Wire: none.

## Related

- [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) - the Rust snapshot and stream side of the same change
