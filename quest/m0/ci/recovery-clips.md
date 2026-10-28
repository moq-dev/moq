# [S] Recovery test clips encode once per run

## Goal

`recovery_test`'s FFmpeg clips, and the reference frames decoded from them,
are produced once per test run instead of once per test process, with every
case and its coverage kept. FFmpeg encode and decode together take 35 to 40%
of its CPU today; the PR reports how much of that each part saved.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile of the slowest tests. Pick one of: committing the clips (about 320 KB
each), generating them in a nextest setup script, or caching them under a
lock keyed by the FFmpeg version. Reusing the encoded clip alone leaves the
per-process reference decode, so share the decoded reference data the same
way or report the encode-only saving. Measure it the way that PR measured its cohort, on the same machine before
and after.

Public API: none.
