# [S] Snapshot consumers always read at zero delay

## Goal

`moq_json::snapshot::Consumer` and `@moq/json`'s snapshot consumer always
subscribe and read at a zero `delay.max`, with no way for a caller to override
it. At budget 0 an owed hole is stale as soon as a newer group arrives, so
`latest()` on a received track returns the newest buffered snapshot at once
instead of waiting for an owed lower group. The `poll_latest` caveat goes.
No new `Ordered` read.

## Plan

Decided 2026-11-03 (maintainer, /quest-unblock): ✅ a snapshot is always
max-delay 0; no new `Ordered` read. It replaces the 2026-10-31 plan of an
`Ordered` read that skips owed holes. Not chosen: `poll_latest_group` /
`latest_group`; `next_available_group`; forcing 0 and also adding the read.

- Both consumers set their subscription's `delay.max` to zero at
  construction, keeping the caller's other preferences. Neither config has a
  delay setting to remove. Add migration notes: a caller that set a budget to
  replay history or trail a playhead now reads only the newest group.
- Delete the `poll_latest` caveat (`rs/moq-json/src/snapshot/consumer.rs`)
  and the budget guidance in both consumers' docs.
- Rust and JS tests: a snapshot reader sees group 2 while group 1 is still
  owed, and `latest()` returns group 2 at once, whatever budget the caller
  subscribed with.
- `Ordered` at budget 0 already passes an owed hole in both languages
  (`next_group_waits_on_an_owed_group_within_the_budget` in
  `rs/moq-net/src/model/track.rs`, and the zero case of
  `ordered owed holes follow zero, narrowed, unlimited, and publisher budgets`
  in `js/net/src/track.test.ts`); fix `Ordered` only if that stops holding.

Public API: snapshot consumers ignore the subscription's `delay.max` (Rust and
JS). Wire: none.
