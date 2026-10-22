# [M] TS export follows a PMT revision

## Goal

A TS export of a broadcast whose layout changes mid-stream announces the
revised layout as an ordinary PMT revision. A track added, retired, or
replaced under a fresh name changes the PMT at the next `version_number`;
nothing else blips.

## Plan

The import already keeps track names honest: a changed `stream_type` or ES
descriptor loop finishes the old track, drops its catalog entry, and opens
a new uniquely named one; a removed stream finishes its track and leaves
the catalog; `programDescriptors` updates in place; a program identity
change refuses.

What the export does today:

- `Export` builds the PAT/PMT once. A track added afterward fails the export
  ("TS track layout changed after PAT/PMT was emitted"), so every revision
  the import publishes ends the export.
- A track that leaves the catalog keeps its PID in the PMT forever, and
  `programDescriptors` is read once.
- A switch to another instance already rebuilds the tables at the next
  `version_number`.

Decided 2026-10-20 by the maintainer:

- Export rebuilds the PMT at the next `version_number` when the track set
  or an entry changes, drops a retired track's PID once its track ends, and
  keeps the PCR PID unless its track retires. A TS receiver sees an
  ordinary PMT revision and a Hang player sees a rendition switch.

A `programDescriptors` change is an entry change too: the PMT carries the
revised `program_info` at the next `version_number`.

Under `moq export ts --sync`, the revision is a function of the catalog
alone, so identical legs revise their PMT identically.

Rust: `rs/moq-mux` TS export. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): the rebuild section says
how a consumer revises the PMT.

Tests use the generated `fixture::Mux` builder through import and export:
PMT add, remove, retype, descriptor-only change, version rollover (31 to
0), PID reuse after removal, and a program descriptor change, each
asserting the exported PMT's streams and version sequence. Extend the
TSDuck round trip with a generated mid-stream PMT revision so
`compliance.py` checks the rebuilt version sequence in CI.

## Related

- [PAT changes](/quest/m1/ts-reliability/pat-lifecycle.md) - a moved PMT PID reaches the export as a new PAT
