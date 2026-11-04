# [M] A waiter's record limit changes no behavior

## Goal

Raising kio's `PARKED`, the number of lists a `kio::Waiter` remembers parking on, from 8 to 16 changes only what it costs: no moq-net test or relay behavior depends on the limit. Today two moq-net integration tests fail at 16 and pass at 8, so a missed wake, or a test leaning on one by accident, hides behind it. Find and fix that, then set the limit to 16 and take the 3.9% per delivered group the chat fan-out measured.

## Plan

Decided 2026-11-03 (maintainer, on [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)): m0, as a correctness bug rather than a performance tweak, since "the fact that tests fail if we change a constant is a HUGE red flag".

Reproduce: set `const PARKED: usize = 16;` in `rs/kio/src/waiter.rs`, then

```bash
nix develop --command cargo nextest run -p moq-net --all-features \
  -E 'test(an_errored_end_keeps_its_history_across_a_hop) | test(a_quiet_catalog_reaches_an_unfloored_relay_reader_when_a_peer_resumes_past_it)'
```

- `errored_end::an_errored_end_keeps_its_history_across_a_hop` fails with `subscribe: Stream(App(7))` at `rs/moq-net/tests/integration/errored_end.rs:35`.
- `quiet_catalog::a_quiet_catalog_reaches_an_unfloored_relay_reader_when_a_peer_resumes_past_it` fails with `resume subscribe: Unroutable` at `rs/moq-net/tests/integration/quiet_catalog.rs:178`.
- Both pass at 8, on [0e9b7ae6bb](https://github.com/moq-dev/moq/commit/0e9b7ae6bb4d22b50da244a3c1146acd9bb91f5a) and on `main` at the time.

The lead: `rs/moq-net/src/util/idle.rs` keeps a read's waiter, and reports the read parked without walking it again, only while every list it parked on is recorded. Past the limit the waiter is marked lost and retired, and the read walks again. At 8 the reads in these tests overflowed and walked on every poll; at 16 they stay parked, so a change they depend on doesn't wake them, through a list not registered with this waiter or a wake that only a walk noticed. Find the read and the change, and fix the wake or the test's hidden dependency, not the limit.

Then set the limit to 16:

- `util::idle::test::a_waiter_parked_past_its_records_is_retired` parks on 9 lists, the old limit plus one; make it the limit plus one.
- The larger waiter makes clippy's `large_enum_variant` fire on `Sub` in `rs/moq-net/src/model/resume.rs` and `Reader` in `rs/moq-stats/src/aggregate.rs`; box their pending variants, as [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)'s reverted commits [101ed5a806](https://github.com/moq-dev/moq/commit/101ed5a806) and [1a5ceeca84](https://github.com/moq-dev/moq/commit/1a5ceeca84) did.
- Measure with `just bench-fanout "<base>" session "64 512"`. On [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605), 16 took the session scenario from 115,000 to 110,524 instructions per group at 64 viewers and from 113,200 to 108,747 at 512 (-3.9%); 12 and 32 each cost about 1-2% more than 16, since every registration searches the records.

Public API: none expected. Wire: none.

## Related

- [Relay CPU per frame since #5926](/quest/m1/release-perf/relay-cpu-bisect.md) - found this, measuring the waiter's registration cost
- [Task sets release memory after a burst](/quest/m2/performance-survey/task-capacity.md) - a task slot's `Park` holds a whole `Waiter`, which 16 records grow
