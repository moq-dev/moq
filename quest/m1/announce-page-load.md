# [M] The live marker waits for the first connection on page load

## Goal

An announcement stream in `@moq/net` treats the `Live` marker like an
`rs/moq-net` stream opened after `connect`:

- Opened before the first session connects, it does not yield `Live` on an
  empty set. `Live` comes only after that first session's initial set has
  been delivered. There is no give-up: a failed connection shows up as a
  connection error, never as an empty `Live`.
- `Live` is a barrier on the prefixes still owed when the last replay hold
  drops: it follows once each of them has been delivered or cancelled. The
  stream keeps draining the live table meanwhile, so a change to an owed
  prefix folds into its pre-`Live` event, a retraction can cancel it, and a
  new prefix that sorts ahead of an owed one can arrive before `Live`.

Every other JS place with the same page-load gap follows the same rule.

## Plan

- Page load (decided): the JS reconnect loop (`js/net/src/connection/reload.ts`)
  already counts as an answerer for requests through `expect()`; it can also
  hold the replay on the origin until its first session's initial set lands.
  The hold never times out. Rust needs no change: a Rust app calls `connect`
  before it opens the stream, which then waits on that session's replay, and
  a stream opened before `connect` is live at once.
- Boundary ordering (decided): match Rust's owed-prefix barrier
  (`OriginConsumerState::landed` records `LiveState::Owed(pending keys)`, and
  `take` keeps draining the shared, lexicographic `pending` map). When the
  last replay hold drops, record the prefixes whose diff is still pending,
  keep emitting from the live table, and yield `Live` once that set is empty.
  No frozen snapshot: the JS stream is already a coalescing diff of the
  table, which is the same model.
- Tests: a caught-up test that opens the stream before the first connection,
  one where the first connection fails and no `Live` arrives, and barrier
  tests for a change to an owed prefix after the hold drops (folded ahead of
  `Live`), a retraction of one (cancelled, `Live` still follows), and a new
  prefix sorting ahead of an owed one (may precede `Live`).
