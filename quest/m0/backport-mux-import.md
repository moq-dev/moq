# [S] Backport the MKV, FLV, and VP9 import fixes to release

## Goal

moq-mux on `release` refuses the malformed MKV, FLV, and VP9 input that
[moq-dev/moq#6908](https://github.com/moq-dev/moq/pull/6908) made fail loud on
main, instead of wrapping timestamps, panicking, or silently dropping tracks,
and publishes a patch release.

## Plan

Decided 2026-10-27 (maintainer): one PR against `release` per backport, in m0.

The bugs on `release` (as of 6ce67943f7):

- `vp9/mod.rs:52,136-137`: `u16` width and height wrap at 65536.
- `flv/import.rs:220,364`: unsupported codecs are warned about and dropped.
- `mkv/import.rs:200`: `HierarchyProblems` allowed.
- `mkv/import.rs:246-247`: a TimestampScale of 0 accepted.
- `mkv/import.rs:298`: unsupported tracks skipped with a log line.
- `mkv/import.rs:314`: a duplicate TrackNumber is not refused.
- `mkv/import.rs:417`: the cluster timestamp plus relative timestamp overflows.

Port each fix with its test. A dropped track that becomes an error is a
behavior change for `release` users; call it out in the changelog.

Public API: none. Wire: none.
