# [M] The live marker waits for the first connection on page load

## Goal

An announcement stream in `@moq/net` matches `rs/moq-net` around the `Live`
marker:

- Opened before the first session connects, it does not yield `Live` on an
  empty set. `Live` comes only after that first session's initial set has
  been delivered. There is no give-up: a failed connection shows up as a
  connection error, never as an empty `Live`.
- A change landing in the same tick as the last replayed route is yielded
  after `Live`, not folded into the snapshot ahead of it.

Every other JS place with the same page-load gap follows the same rule.

## Plan

- Event names: a separate dev PR renames the variants to
  `Start`/`Update`/`End`/`Live` (today `announced`/`updated`/`retracted`/`live`).
  Use the new names.
- Page load (decided): the JS reconnect loop (`js/net/src/connection/reload.ts`)
  already counts as an answerer for requests through `expect()`; it can also
  hold the replay on the origin until its first session's initial set lands.
  The hold never times out. Rust already behaves this way and needs no change.
- Boundary ordering (decided): the JS stream is a coalescing diff of the
  table, so a same-tick change is merged into the snapshot ahead of `Live`
  ([#4261 discussion](https://github.com/moq-dev/moq/pull/4261#discussion_r4113867903)).
  Snapshot the table when the last replay hold drops, yield that snapshot,
  then `Live`, then diff from the snapshot, as Rust does.
- Tests: a caught-up test that opens the stream before the first connection,
  one where the first connection fails and no `Live` arrives, and a same-tick
  test where a change lands with the last replayed route and comes after
  `Live`.
