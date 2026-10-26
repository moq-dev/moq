# [M] Session-only model items stay private

## Goal

moq-net and `@moq/net` model items that only the built-in sessions use are
private. The public model serves applications: publish, subscribe, and answer
on demand.

## Plan

Decided 2026-10-23 (maintainer): the old public-model goal, "a third party
builds its own publisher, subscriber, or forwarding relay from the public
model alone", is dropped. It made every item a built-in session reaches
public, with no consumer outside the sessions. Items go public again only
for a named consumer.

- Revert the session-only items
  [moq-dev/moq#6111](https://github.com/moq-dev/moq/pull/6111) made public:
  `track::Producer::{received, drop_groups, close}`,
  `track::Request::{request_start, stage}`, `track::Staged`, and
  `group::Producer::info` (it duplicates `Deref<Target = Info>`) become
  `pub(crate)`. JS `Track.Request.stage`, `Track.Staged`, and
  `Track.Producer.dropGroups` become internal again (`dropGroups` returns to
  `hooks.dropTrackGroups`). Keep `create_frame(self)`: hang and moq-mux use it.
- The `// pending:` and `// private:` marks and their lint are already gone.
  The moq-transport passthrough fields stay crate-private.

Rejected: the toy-transport integration tests proving a third party could
build from the public model; the goal they proved is dropped.

Public API: moq-net and `@moq/net` removals, all main-only (none reached
`release`), so no migration note; the Impact bullet flags the unreleased #6111 entry
for the cut to drop.
Wire: none.
