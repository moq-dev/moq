# [S] A lowered resume floor re-offers a released copy's groups

## Goal

Lowering a resumed reader's floor with `with_range` (JS `withRange`) offers
every group below the old floor that some route still caches and the reader
never read, as it does for a direct reader. Today, in Rust, a reader that
switches routes while its floor is high lets go of the replaced copy that
cached the lower groups, so a later lowering re-offers only what the serving
copy holds: `Copy::start` clamps the requested floor to that copy's resume
point.

## Plan

Found by Codex on [moq-dev/moq#6138](https://github.com/moq-dev/moq/pull/6138),
which made lowering a floor assign. Its Rust test covers one copy only.

Open decision, to settle before implementing:

- Recommended: once the floor drops below a released copy's groups, ask the
  serving route for them on demand, rather than keeping every replaced copy
  alive in case the floor drops later, which holds memory for a lowering that
  may never come.
- Alternative: narrow the `with_range` contract to the copies the reader still
  holds, and document it.

Check the JS resume reader for the same gap, and cover both with a two-copy
test: switch routes with a high floor, release the old copy, lower the floor,
and expect the groups only it cached.

Public API: behavior only. Wire: none.
