# [M] The archive format ships as version 2, and readers accept only it

## Goal

The stored format `moq-archive` writes at the cut is version 2, the
successor to the version 1 that `release` ships, and it is one format: the
reader accepts exactly version 2 and names the version it refused. The code
for the layouts main called 2, 3, and 4, none of which was ever published,
is gone, and `rs/moq-archive/src/lib.rs`'s format description matches what
the reader uses.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Ship as version 2, and the reader accepts exactly it (rejected:
  bumping to 5 as the one version at the cut; keeping 4 and deleting only
  the legacy paths; freezing as is). The number counts published formats:
  2, 3, and 4 only ever existed on main.

Facts found by the audit:

- `rs/moq-archive/src/lib.rs` has `VERSION = 4` and `VERSION_MIN = 2`;
  `origin/release` has `VERSION = 1`, a different layout that main already
  cannot read.
- The reader still carries the unpublished layouts:
  `rs/moq-archive/src/layout.rs` (`LEGACY_GROUP`, `legacy = version < 4`,
  the three-field entry) and `rs/moq-archive/src/info.rs` (the version 2
  priority byte, and the `VERSION_MIN..=VERSION` range checks in both
  files).
- [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638) changed
  what version 4 means without bumping it: the entry sequence and the
  key's group field became the group's start
  (`rs/moq-archive/src/producer/track.rs`, `let sequence = group.start`).
  A version 4 archive written before it and one written after disagree.
- The object key is `<group>-<frame>-<pts>`. Since #8638 the consumer
  drops the third field: `rs/moq-archive/src/consumer/index.rs` keeps
  `(start.group, start.frame)` and says "a group's sequence is its start,
  so these sort by time too", while `lib.rs` still says the listing
  "answers group and time lookups". The producer reads `pts` back to
  resume (`producer/track.rs`).
- `.info` and the layout structs use `deny_unknown_fields`
  (`info.rs`, `layout.rs` twice), so no field can be added without a
  version bump.

Recommendations for the PR (not decided):

- The key's `<pts>`: use it or remove it. A field every implementer must
  write and validate, and no reader uses, is what a freeze makes
  permanent. Removing it needs the producer's resume to recover the
  timestamp another way (the object's first frame); if that costs a GET
  per resume, keeping the field and making the consumer's time lookup use
  it is the better end. Say which in the PR.
- Tolerate unknown fields in `.info` and the layout records, so version 2
  can gain an optional field without becoming version 3. Keep
  `deny_unknown_fields` only where an ignored field would change what the
  bytes mean.
- Decide on purpose whether per-frame timestamps stay absolute while
  sequences are delta coded; the audit had low confidence it matters.

For the PR:

- First check moq.pro for stores written by a main build (its staging
  recorder): those archives say 2, 3, or 4 and mean something else. The
  format is unpublished, so they are rewritten or discarded, not read;
  the Impact section says so. A deployed store that must stay readable is
  the one thing that reopens the decision.
- A version 1 archive stays unreadable, as on main today; the error names
  the version found and the one supported, so nobody mistakes a release
  archive for a corrupt one.
- Update `doc/concept/archives.md` and the crate docs to the one format.

Tests: a version 2 archive round-trips through producer and consumer; an
`.info` or layout that says 1, 3, or 4 is refused with the typed version
error; resume after a restart finds the last object; if unknown fields are
tolerated, a record with an extra field reads.

Public API: none expected beyond the version constants. Wire: the stored
archive format; archives written by any main build before this are not
read.

## Related

- [Media consumers address groups by start](/quest/m2/time-addressing/consumers.md) - the model that made the key's group field a start
