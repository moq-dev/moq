# [S] moq-net's concurrent tests run under many Miri schedules and ThreadSanitizer

## Goal

Nightly runs moq-net's multi-threaded tests under Miri across many scheduler
seeds, and its whole library test suite under ThreadSanitizer. A data race or
an ordering bug in its locks, atomics, and wakers fails Nightly, with no change
to the code under test.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net). Milestone m4.

- ✅ Shim-free tools only (rejected: Shuttle, and loom back for kio alone).
  Loom was removed in [moq-dev/moq#7474](https://github.com/moq-dev/moq/pull/7474)
  because it never found a race and needed a `cfg(loom)` shim around every
  primitive; Shuttle needs the same shims. These two run the tests as they
  are, so there is nothing to keep in sync.

Decided 2026-11-27 (maintainer, /quest-plan on kio's follow-ups): ✅ narrow
to moq-net (rejected: drop the quest). kio's half shipped in
[kixel-ai/kio#25](https://github.com/kixel-ai/kio/pull/25), in kio's own
post-merge CI; moq-net's locks and wakers still get neither tool. Resized
from M to S, since that PR's recipes are the pattern to copy.

- Copy kio's `miri-seeds` and `tsan` recipes from its
  [justfile](https://github.com/kixel-ai/kio/blob/main/justfile): 8 seeds over
  an exact list of the tests that spawn threads, failing when a name stops
  matching, and a TSan run of the library tests with `-Zbuild-std` and an
  explicit `--target`.
- Today `sh/rs/miri.sh` runs each test once, `rs/justfile` has only `miri`,
  Nightly runs `rs miri` alone, and nothing runs TSan. Widen the Miri pass past
  `model::frame`, `model::group`, and `path` where a module shares state
  across threads (`grep -rln 'thread::spawn\|new_multi_thread' rs/moq-net/src`).
  Choose the seed count from the measured time.
- A TSan suppression needs a comment naming the false positive.
- Both are random, not exhaustive: a pass is evidence, not a proof. Say so in
  the recipes' doc lines.

Public API: none. Wire: none.
