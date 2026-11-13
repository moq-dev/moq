# [S] Miri's GenMC mode explores every schedule of kio's lock-free code

## Goal

Nightly runs kio's log and channel tests under Miri's GenMC mode, which
explores every thread schedule and weak-memory outcome the Rust memory model
allows, with no change to the code under test. An ordering bug in kio's
lock-free code fails Nightly on every run, not only on a lucky seed.

## Waiting

- rust-lang/miri: a Miri shipped as a rustup component runs `cargo miri test`
  in GenMC mode (`-Zmiri-genmc`). Check
  [Miri's GenMC document](https://github.com/rust-lang/miri/blob/master/doc/genmc.md),
  which today says `cargo miri test` is not supported and GenMC needs a Miri
  built with its own feature flag. Delete this quest if upstream drops the
  mode.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#8472](https://github.com/moq-dev/moq/pull/8472)): ✅ plan it in
m5, waiting on upstream (rejected: dropping it and relying on random seeds
alone).

Loom did this job and was removed in
[moq-dev/moq#7474](https://github.com/moq-dev/moq/pull/7474) because it
needed a `cfg(loom)` shim around every primitive and never found a race.
GenMC needs no shims, so the only upkeep is the Nightly row.

- Add a pass to `sh/rs/miri.sh` over the tests of `rs/kio/src/log.rs` and
  `rs/kio/src/channel.rs` that spawn threads. Keep the many-seeds pass for
  everything GenMC cannot run.
- GenMC explodes with thread count and loop bounds. Size each test it runs
  to a few threads and a few operations, and measure that the pass fits
  Nightly.
- As of 2026-11-10 it also misses bugs that depend on reused allocations and
  on spurious `compare_exchange_weak` failures, and it runs without borrow
  tracking. Recheck those limits and say in the recipe which remain.

Public API: none. Wire: none.

## Required

- [Concurrent tests run under many Miri schedules and ThreadSanitizer](/quest/m4/miri-seeds-tsan.md) - the Miri pass over threaded tests that this makes exhaustive

## Related

- [Drop kio's epoch fence](/quest/m5/crossbeam-pin-fence.md) - the ordering in kio's log that only a weak-memory check can confirm
