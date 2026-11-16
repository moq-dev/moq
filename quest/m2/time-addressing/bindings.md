# [S] Bindings address groups by start

## Goal

moq-ffi, `moq-c`, and every language binding expose groups by start
timestamp instead of sequence number, including `seek` for GOP-aligned
failover, with each `doc/lib` page and the PR's Impact section saying how to migrate.

## Plan

Follow the cross-package sync table from `rs/moq-ffi` outward. Timestamps
already cross the bindings as `{value, scale}`, so a start reuses that
type. Decided 2026-11-03 (maintainer, replacing 2026-10-27's "producer arguments
keep their optional timestamp"): every read and write carries a timestamp.
Reads drop the `Option` that exact timestamps keeps for untimed, producer
arguments take a required one, and nothing is wall-clock stamped. A
scale-less track's frames carry their group's key. See
[the model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md).

Resized from M to S 2026-11-14 (the m2 triage before the cut, under
/quest-afk, for the maintainer's review): [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)
already makes every binding's timestamps required and fills each group field
with the start, so what remains is the names. moq-ffi still says `sequence`
where it now means the start: the group record's field, `fetch_group`'s
argument, `seek`, and the docs on the ordered and arrival readers
(`rs/moq-ffi/src/consumer.rs`, `media.rs`). Rename them to `start` the same
way in moq-ffi, `moq-c`, and every binding, mirroring Rust's `group::Info`.
It stays in m2 because the rename is breaking and the cut would otherwise
publish a `sequence` that holds a start.

Found 2026-11-16 (/quest-plan of the pre-cut audit): that inventory is
short. The producer side says `sequence` too, in
`rs/moq-ffi/src/producer.rs`: `MoqFill::accept`, `is_wanted`, and `unwanted`
take `sequence` (`:582-601`); `MoqTrackProducer::create_group(sequence)`
(`:859`) already passes it as `group::Info { start }`;
`finish_at(final_sequence)` (`:950`); `MoqGroupProducer::sequence()`
(`:1002`, filled from `group.start` at `:993`); and `MoqPosition.group`'s
doc calls it "The group sequence" (`:17`). The hand-written C API has
`moq_group_request_sequence` (`rs/moq-c/src/api.rs:4035`) and
`moq_publish_group_sequence` (`:4154`). Rename all of them in the same PR,
then grep every wrapper and `doc/lib` page for `sequence` once more: a name
left behind is a second breaking release.

Decided 2026-11-16 (maintainer, same run): the hand-written C API ships as
`libmoq` ([its name](/quest/m2/libmoq-name.md)), so where this quest says
`moq-c` it means that crate, whichever name it carries when this lands. The
JS half renames a frame's `sequence` to `index`
([JS](/quest/m2/time-addressing/js.md)); a frame's position in the bindings
takes the same name if any binding exposes one.

Public API: breaking in every binding. Wire: none.
