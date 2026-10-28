# [S] Recovery test clips encode once per run

## Goal

`recovery_test`'s FFmpeg clips are encoded once per test run instead of once
per test process, cutting the 35 to 40% of its CPU that FFmpeg encode and
decode take today, with every case and its coverage kept.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile of the slowest tests. Pick one of: committing the clips (about 320 KB
each), generating them in a nextest setup script, or caching them under a
lock keyed by the FFmpeg version. Measure it the way that PR measured its cohort, on the same machine before
and after.

Public API: none.
