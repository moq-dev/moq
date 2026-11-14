# [M] Concurrent tests run under many Miri schedules and ThreadSanitizer

## Goal

Nightly runs kio's and moq-net's multi-threaded tests under Miri across many
scheduler seeds, and their whole library test suites under ThreadSanitizer.
A data race or an ordering bug in the locks, atomics, wakers, and kio's
lock-free log fails Nightly, with no change to the code under test.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net). Milestone m4.

- ✅ Shim-free tools only (rejected: Shuttle, and loom back for kio alone).
  Loom was removed in [moq-dev/moq#7474](https://github.com/moq-dev/moq/pull/7474)
  because it never found a race and needed a `cfg(loom)` shim around every
  primitive; Shuttle needs the same shims. These two run the tests as they
  are, so there is nothing to keep in sync.
- Miri: `sh/rs/miri.sh` runs each test once. Add a pass over the tests that
  spawn threads with `-Zmiri-many-seeds`, each seed a different schedule and
  a different weak-memory outcome. Choose the seed count from the measured
  time, and widen the moq-net filter past `model::frame`, `model::group`, and
  `path` where a module shares state across threads.
- ThreadSanitizer: a `just rs tsan` recipe builds std with the sanitizer
  (`-Zsanitizer=thread -Zbuild-std`) and runs the kio and moq-net library
  tests at near-native speed, reaching the tests Miri ignores for foreign
  calls or cost. A suppression needs a comment naming the false positive.
- Both are random, not exhaustive: a pass is evidence, not a proof. Say so in
  the recipe's doc line.
- Miri's GenMC mode would explore every schedule without shims, but does not
  support `cargo miri test` today. It is not part of this quest.
  [Its own quest](/quest/m5/miri-genmc.md) waits on that.

Public API: none. Wire: none.

## Related

- [Wire fuzz coverage](/quest/m1/wire-fuzz-coverage.md) - adds moq-uring and moq-sock to the same Miri recipe
- [Drop kio's epoch fence](/quest/m5/crossbeam-pin-fence.md) - the ordering in kio's log these runs watch
