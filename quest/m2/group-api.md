# [L] Writing a group needs no invented start, and its end has one word

## Goal

A publisher opens the next group without computing its start, ends a group
with `finish`, and ends a track in error with a call of its own:

- `track::Producer::append_group` takes an optional start. None means the
  next start, one past the largest the track holds, so a track with no media
  clock (a catalog, JSON, chat, input) never restates a number.
- `group::Producer::finish()` ends a group without declaring where it ends
  (today's `close()`), and `finish_at(end)` ends it at a declared end
  (today's `finish(end)`), the pair `track::Producer` already has.
- `track::Producer::finish_at(end)` is always a clean end. The errored end
  that keeps the track's history is a separate call that takes the error.
- `track::Fill` is an enum the handler matches: a run of groups to serve, or
  a time lookup that owes no group.

Rust, `@moq/net`, moq-ffi, moq-c, and every wrapper carry the same shapes
and names.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `append_group` takes an optional start, none meaning next (rejected: a
  public `append_next()` beside `append_group(start)`; keeping the required
  start and documenting the idiom). It is the crate's existing convention
  (`create_track(name, None)`, `subscribe(None)`), and JS and moq-ffi already
  take no argument.
- ✅ A group's `close()` becomes `finish()` and `finish(end)` becomes
  `finish_at(end)` (rejected: one `finish(end: impl Into<Option<u64>>)`;
  keeping `close()`). `close` already means releasing a handle or ending a
  broadcast for good, and moq-ffi and JS already say `finish`.
- ✅ The errored end is its own call instead of
  `finish_at(end, Some(err))` (rejected: keeping the optional error).
  [moq-dev/moq#6750](https://github.com/moq-dev/moq/pull/6750) made `finish`
  the clean end and `abort` the error.
- ✅ `Fill` becomes an enum (rejected: keeping one type told apart by
  `time()`). AGENTS.md asks for one enum whose variants carry their data.

Facts found by the audit, counted again at `80ca9787e3` over `rs/` outside
`rs/moq-net/`:

- `track.largest().map_or(0, |largest| largest.group + 1)` is written out
  108 times in 38 files, in production in moq-audio, moq-boy, moq-json, and
  moq-mux; moq-ffi (`rs/moq-ffi/src/producer.rs:105`) and moq-c
  (`rs/moq-c/src/publish.rs:117`) each keep a private `next_start` for it.
  moq-net already has the operation: `append_next`
  (`rs/moq-net/src/model/track.rs:4366`) is `pub(crate)` and test-only.
- `group.close()` appears 252 times; `group.finish(end)` on a
  `group::Producer` 3 times, one in production
  (`rs/moq-mux/src/container/producer.rs:807`). The other 124
  `group.finish()` lines are wrapper types of other crates.
- moq-ffi's `MoqGroupProducer::finish()` calls Rust's `close()`
  (`rs/moq-ffi/src/producer.rs:1065`), and JS has `finish()`
  (`js/net/src/group.ts:706`), so only Rust changes its group names.
- The three production callers that pass an error to `finish_at` each
  compute the live edge first
  (`rs/moq-mux/src/container/producer.rs:1079`,
  `rs/moq-audio/src/encode/producer.rs`,
  `rs/moq-archive/src/consumer/track.rs:138`).
- Every fill handler branches on `Fill::time()` before anything else
  (`rs/moq-archive/src/consumer/track.rs:148`,
  `rs/moq-ffi/src/producer.rs:493`, `rs/moq-c/src/publish.rs:1049`), and a
  lookup's `wanted` is `None` and `is_wanted` false throughout
  (`rs/moq-net/src/model/track/fill.rs:949`).

For the PR:

- `append_group(start: impl Into<Option<u64>>)`: existing calls with a `u64`
  still compile. Delete `append_next`, the 108 copies, both `next_start`
  helpers, and what `rs/moq-mux/src/start.rs` (85 lines) keeps only for this.
  The first group of an empty track starts at 0, as the idiom did.
- `finish()` keeps `close()`'s meaning: every frame written reaches its
  readers, and they cannot tell the group is whole. Say so in its docs, since
  [media consumers](/quest/m2/time-addressing/consumers.md) read the declared
  end `finish_at` writes.
- Recommended name for the errored end: `fail(err)`, ending at the live edge,
  which all three callers want; add an end argument only if a caller needs
  one. `abort` keeps discarding history. The name is the PR's to settle and
  must mirror across languages (JS `finishAt(end, error?)` at
  `js/net/src/track.ts:2212` loses its second argument).
- Recommended `Fill` variants: `Groups` and `Lookup`, so a lookup has no
  `wanted`, `drop_groups`, or `end` to misuse. `MoqFill` in moq-ffi and the
  moq-c fill handle take the same split.
- A scale-less group's `write_frame` still restates the group's start as
  `Timestamp::unscaled(key)` (102 lines outside moq-net). Not decided here;
  raise it in the PR if the optional start makes the restatement removable
  for free.
- Rename the remaining `key` parameters that mean a group's start to `start`
  where this PR touches the signature.
- [JavaScript](/quest/m2/time-addressing/js.md) renames `sequence` to `start`
  and mirrors these shapes, as the bindings already do, not the Rust they
  replaced. Each binding's `MIGRATE.md` note rides this PR's Impact section.

Tests: an empty track's first `append_group(None)` starts at 0 and the next
is one past the largest; a group ended by `finish()` reads whole with no
declared end; a track ended by the errored call serves every group below its
end and then yields the error. Run `just test interop --all`.

Public API: breaking in Rust (`group::Producer::{close, finish}`,
`track::Producer::finish_at`, `track::Fill`), JS (`finishAt`), and the fill
handles of moq-ffi and moq-c. Wire: none.

## Related

- [@moq/net addresses groups by start](/quest/m2/time-addressing/js.md) - mirrors these names
- [Media consumers address groups by start](/quest/m2/time-addressing/consumers.md) - reads what `finish_at` declares
- [Refresh a track by interval](/quest/m2/refresh-interval.md) - changes `track::Demand` beside these calls
