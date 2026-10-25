# [S] Dart leak tests measure moq's memory

## Goal

The three RSS checks in `dart/moq_ffi/test/leak_test.dart` (lines 15, 36,
66) fail only when moq leaks, never from Dart heap growth or allocator
retention under load.

## Plan

Each compares `ProcessInfo.currentRss` before and after 2000 calls of 64 KiB
and asserts growth under 32 MiB, with no forced GC; `dart_test.yaml`
already runs them with `concurrency: 1`. Measure what moq itself owns, such
as a native allocation counter or live-handle count exposed for tests,
instead of whole-process RSS.

Public API: none. Wire: none.
