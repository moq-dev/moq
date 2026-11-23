# [L] Dropping an on-demand track's producer is safe

## Goal

A publisher serving a track on demand ends it by dropping its producer once
nobody wants it, with no race: a track nobody wants closes cleanly, and a
reader that joined since the publisher saw demand go idle is served by a fresh
request for the track instead of reading `Error::Dropped`. Ending an on-demand
track needs no API beyond `Demand` and drop.

## Plan

Today a plain drop races a joining reader, which reads `Error::Dropped`, so
`track::Producer::abort_unwanted` exists to end a track atomically: wait for
`Demand` to go idle, try the abort, and on `Err(kept)` wait again on a fresh
`Demand`. Every producer wrapper forwards it (about 30: each `moq-mux` codec
importer and container, `moq-json`'s snapshot, window and stream producers,
`moq-audio`, `moq-video`, `moq-ffi`, `libmoq`), and every on-demand app
rewrites the same idle loop around it; a downstream deployment hit the race
as an end-to-end flake and carries a local helper for that loop. The sharp
edge is the drop, so fix the drop rather than add another helper.

Decided 2026-11-23 (maintainer): ✅ make drop safe and delete the API
(rejected: a `Producer::until_unwanted` helper over `abort_unwanted`, which
each wrapper would forward too; leaving upstream unchanged).

Recommendations (taken while the maintainer was away; open to review):

- When the last producer of a track drops, under the same lock consumer
  creation takes: if nobody wants the track, close it as `abort_unwanted`'s
  `Ok` does today, with no error for existing handles' final reads. If a
  reader joined in the race, hand that reader's lookup back to the broadcast
  as a fresh `track::Request` (as a later lookup already requests the track
  afresh), so the publisher's request loop serves it again and the reader
  waits for the new producer instead of failing.
- A track created with `create_track` rather than accepted from a request
  follows the same rule, since a later lookup of its name already becomes a
  request.
- This is a behavior change with no API break, so it lands in m3 ahead of
  [deleting `abort_unwanted`](/quest/m3/abort-unwanted-delete.md), which is the
  breaking half.
- Model-check the drop against a joining reader (the `verify-*` skills cover
  neighbouring state machines); test at `moq-net`'s model layer that a reader
  joining between the idle report and the drop is served by the next
  producer, and that an unwanted drop leaves no `Error::Dropped`.
