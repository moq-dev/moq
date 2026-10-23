# [S] HLS cursors error on every abrupt teardown

## Goal

moq-hls's recorder cursors (`export::renditions::Consumer` and
`export::segments::Consumer`) return `Ok(None)` only for a complete
recording. Every other way a window closes, such as a retired rendition or a
broadcaster closed without finishing, ends the cursor with an error, each
path covered by a test.

## Plan

Decided 2026-10-23 (maintainer interview), following
[moq-dev/moq#6468](https://github.com/moq-dev/moq/pull/6468), which made a
failed timeline an error but left a window that closes without ending or
failing returning `Ok(None)`:

- Audit every path that closes a window and decide each as either a clean
  finish or an error; a recorder must never mistake a truncated recording
  for a complete one.
- The cursors get a real consumer in
  [the CLI recorder](/quest/m1/hls-record-cli.md).
- Update `rs/moq-hls/MIGRATE.md` for the changed end semantics.

## Related

- [HLS recorder in the CLI](/quest/m1/hls-record-cli.md) - the cursors' first consumer
