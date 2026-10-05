# [S] A JSON stream refuses an oversized record without ending

## Goal

A `@moq/json` or `moq-json` Stream append that cannot fit the group budget
(32 MiB or 8192 frames) throws `GroupTooLarge` and leaves the log intact,
compressed or not. A JS subscriber to a track that is gone gets `NotFound`, as
in Rust, instead of waiting forever.

## Plan

Today `js/net/src/group.ts` wipes and closes the group on overflow and
`js/json/src/stream/producer.ts` aborts the track, which ends the log for
every reader. Ending the log is deliberate for a failed write
(`rs/moq-json/src/stream/mod.rs`), but a refused record is not a hole.

Decided (2026-10-04):

- Before encoding, check the record's raw size plus deflate's worst-case
  overhead against the remaining budget. Encoding advances the DEFLATE window,
  so a check after encoding would desync readers. The budget is tracked
  inside @moq/json and moq-json, not exposed by moq-net. A failure past the
  check still aborts the track.
- The budget covers the whole log, so once it is spent every append throws.
  The check spares the log from one oversized record; it does not extend a
  full one, which still needs a new track.
- Track lifetime, as Rust already behaves: a finished track stays cached and
  new subscribers get it from the cache; only a dropped or aborted producer
  removes it, after which a subscribe with no handler alive answers
  `NotFound`. `js/net/src/broadcast.ts` queues an unserved request instead;
  make it match, and check that JS keeps a finished track servable.
- Document the budget in `doc/lib/js/json.md` and the Rust crate docs.

Tests: an oversized record throws and the next record is readable, in both
compression modes; a JS subscribe to an aborted local track errors with
`NotFound`; a finished track is still served.

## Closes

- [#4771](https://github.com/moq-dev/moq/issues/4771) - close this issue when the quest finishes
