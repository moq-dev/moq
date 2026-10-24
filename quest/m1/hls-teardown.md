# [M] HLS cursors error on every incomplete recording

## Goal

moq-hls's recorder cursors (`export::renditions::Consumer` and
`export::segments::Consumer`) return `Ok(None)` only for a complete
recording. Every other way a window closes, such as a broadcaster closed
without finishing, ends the cursor with an error, as does a segment the
cursor misses because it fell behind. A rendition the catalog removes or
reconfigures is a supported transition, not an error: its cursor drains and
ends cleanly, and the renditions cursor reports `Removed`. Each path is
covered by a test.

## Plan

Decided 2026-10-23 (maintainer interview), following
[moq-dev/moq#6468](https://github.com/moq-dev/moq/pull/6468), which made a
failed timeline an error but left a window that closes without ending or
failing returning `Ok(None)`:

- Audit every path that closes a window and decide each as either a clean
  finish or an error; a recorder must never mistake a truncated recording
  for a complete one.
- Raised in review, scope accepted by the maintainer: a slow cursor misses
  media silently today. `segments::State::next_after` resumes at the oldest
  row still in the window, and `Consumer::next` skips a segment whose groups
  left the relay cache. Both become errors, with a slow-consumer test; a
  segment that is a gap for the rendition stays a skip.
- Kept for static VOD export, 2026-10-23 (maintainer): a directory that plays
  from a plain CDN with no moq server. Recording-then-serving is already
  covered by moq-archive.
- The cursors get a real consumer in
  [the CLI recorder](/quest/m1/hls-record-cli.md).
- Update `rs/moq-hls/MIGRATE.md` for the changed end semantics.

## Related

- [HLS recorder in the CLI](/quest/m1/hls-record-cli.md) - the cursors' first consumer
