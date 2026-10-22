# [S] The HLS segment cursor reports a failed timeline

## Goal

`moq-hls`'s export `segments::Consumer::next` drains the segments a failed
timeline still covers, then returns the failure as an error. `Ok(None)`
means only a clean end, so a recorder that mirrors a broadcast to storage
can't take a truncated recording for a complete one.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): fix it before the
cut, since changing what `Ok(None)` means is a breaking change.
[moq-dev/moq#6153](https://github.com/moq-dev/moq/pull/6153) fixed the
playlist side and named this follow-up.

In `rs/moq-hls/src/export/segments.rs`, `Consumer::next` returns `Ok(None)`
when `rendition.is_failed(&row)`, and its doc comment says so. A shared
reference failure ends the same way: `Producer::fail` records `failed`
without setting `ended`, the closed window maps to `Ready(None)`, and
`next_after` never reads `failed`. `Producer::failure()` holds the error
nobody reads.

Return the window's or rendition's failure once the covered segments drain.
Fix the doc comment and add a `rs/moq-hls/MIGRATE.md` entry. Test the
cursor alongside the playlist for both failure sources: valid segments
first, then the error, never `Ok(None)`.

The cursor has no consumer outside its tests yet. It stays public because
it is the documented entry point for mirroring to storage.

Public API: `segments::Consumer::next` returns an error where it returned
`Ok(None)` (breaking). Wire: none.
