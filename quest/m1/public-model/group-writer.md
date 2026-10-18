# [M] One group writer

## Goal

`moq_net::group::Producer` is not `Clone`. Each group has exactly one
writer, so every frame write, `finish`, and abort on it is ordered by
ownership and `&mut self` rather than by lock ordering. The track cache, the
reader handoff, and the built-in sessions hold a crate-private handle that
cannot write or finish. No reader-visible change except the teardown below,
and no wire change.

## Plan

Follow-up to #5725 (live largest), chosen by the maintainer there: its
decision 10 ordered frame writes against `finish` with the group lock because
a frame write on one clone could race `finish` on another. Base the work on
`main` after #5725 lands.

This is the "drop public `Clone`" half of the owned frame writer decision in
[Public ingest](/quest/m1/public-model/ingest.md) (settled 2026-10-16,
#5717), split out because it stands alone. Ingest keeps the public owned
chunked writer (`create_frame(self)`); this quest only makes it possible.

What clones a group producer today (compile probe on #5725's head, deleting
`impl Clone`):

- Outside moq-net, tests only: moq-ffi `MoqGroupProducer::used`,
  `rs/moq-net/benches/track.rs` (2), and `rs/moq-net/tests/loom.rs`.
- moq-net unit tests: `group.rs` (6), `track.rs` (2), the IETF publisher
  (1) and subscriber (1).
- 13 library sites in moq-net, in four groups:
  - Track cache slots (`Slot::group` and the fetched `head` in `track.rs`,
    from `insert_group`): cache accounting, eviction, demotion, `bound`,
    reads, and abort.
  - Reader handoff: `poll_recv_group` and `poll_next_in_range` return a
    clone that the caller turns into a consumer.
  - Abort through a shared reference, because `abort(self)` consumes:
    `Slot::abort`, track abort, the IETF subscriber's fill, and
    `group::Producer::frame_abort`.
  - Wire ingest: `create_frame_owned` (`frame::ProducerOwned` holds a clone
    across polls), the IETF ingest's `writing` clone, the observers for
    `demand` (`poll_unused`, `abort_unused`) and `closed` (`poll_closed`),
    and the lite subscriber's `reveal_group`.

Shape:

- One public writer, `group::Producer`, with no `Clone`.
- A crate-private `group::Handle` minted from the writer that reads group state,
  hands out consumers, waits on `poll_closed` and `poll_unused`, aborts
  through `&self` (`abort`, `abort_unused`), and backs the cache slot's
  accounting and `reveal_group`. It has no write or finish. The 13 sites
  above move onto it. It stays crate-private until a third-party session
  needs to abort or watch a group it does not own; the ingest decision
  already says that need gets a non-writing handle, never `Clone`.
- `frame::ProducerOwned` takes the writer by value and hands it back when
  the frame finishes, instead of cloning it. The wire drivers keep the
  handle for `poll_closed` and demand while a frame is open. Making that
  owned writer public stays with ingest.
- `answer.md`'s public `poll_unused` and `abort_unused` stay on the writer;
  the handle carries the same calls for the sessions.
- Once one writer orders writes against `finish`, simplify the group-lock
  ordering #5725 added for that race if nothing else needs it, and replace
  the loom model `a_frame_racing_finish_never_reopens_the_largest` (it
  races two handles, which no longer compiles) with a `compile_fail`
  doctest that `let _: group::Producer = producer.clone();` is refused.
  Keep any loom coverage that still races a writer against the handle's
  abort.

Behavior change to verify: the "dropped without `finish()` or `abort()`"
teardown (`group::Alive`'s `Drop`, which warns and releases the cached
frames) runs today only when the last clone drops, and the cache slot holds
one, so a writer that drops an unfinished group leaves its readers waiting
until the slot goes. After the split it runs when the writer drops. Split
`Alive` so the handle shares the atomics (`aborted`, `cut`, `checked`,
`access`) but not the drop guard. Then check, each with a mock-time test:

- A reader of an unfinished group sees `Dropped` when its writer drops, and
  the cache releases the frames.
- A route that dies mid-group still resumes on its replacement from the
  committed frame (`resume_frame`, `resume_position`), which reads the count
  that outlives the released cache.
- The sessions' `crate::recv::Group` guard and moq-ffi's
  `MoqGroupProducer` still end every group they drop with a terminal.

If any of those reads worse than today, bring it to the maintainer before
landing.

Decided by the maintainer on 2026-10-18 (#5892):

- m1, under the public-model epic, since #5717 already put the Clone
  removal there. The public owned chunked writer stays in ingest, which
  requires this quest.
- The handle is `group::Handle`.
- Delete the public `Error::FrameOpen`: one writer makes it unreachable.
  The in-flight-frame check becomes a `debug_assert`. Update any remaining
  references to it.
- `finish(&mut self)`, like every other write.
- Keep `Deref<Target = Info>`. A downstream `producer.clone()` now silently
  copies the `Copy` `Info` and fails later, so MIGRATE.md names that.

Landing: breaking public API (no `Clone`, no `FrameOpen`, `finish` takes
`&mut self`), so `rs/moq-net/MIGRATE.md` gets an entry under
`## Unreleased` naming the `Info` pitfall, plus moq-ffi's if
its exported surface changes (it should not). JS has no equivalent: a JS
`Group.Producer` is a single object and needs no change. Docs: none beyond
MIGRATE, since `doc/` never documents cloning a group producer.

## Required

- [Live largest location](/quest/m1/largest.md) - #5725, in the merge queue; this quest builds on its group-lock ordering and loom model
