# [S] Replay provenance

## Goal

The catalog a replay publishes carries the root `archive` entry the line
promises: the timeline track, the replay path, the store URL, and the format
version. A player or tool can tell a replay from the live source and find the
recording.

## Plan

- Check which of these fields `rs/hang` and `js/hang` already define. Add any
  missing ones as optional fields in both, and in the Recording section of
  `drafts/draft-lcurley-moq-hang.md`.
- Never advertise credentials: strip userinfo and query from the store URL, and
  let the importer opt out of advertising the URL at all.
- `moq import archive` fills the entry. Test that a round trip through
  `export archive` and `import archive` advertises it.
