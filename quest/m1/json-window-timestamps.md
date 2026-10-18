# [S] JSON window consumers return each event's timestamp

## Goal

The moq-json and `@moq/json` window consumers return each event with a
timestamp. The Rust and JS snapshot and stream consumers already return
`Timed<T>` (#5099, #5677); the window consumer is the last one.

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
  `## Unreleased` of each affected package's `MIGRATE.md`. No new doc page.
- Land before the next `@moq/json` release, so its break ships together with
  #5099's. The Rust break uses the same `moq_net::Timed` as #5677 and ships in
  the next moq-json release with it.

Public API: breaking (Rust and JS). Wire: none.
