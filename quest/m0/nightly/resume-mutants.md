# [M] No resume or rekey mutant survives the Nightly mutation run

## Goal

The Nightly `mutants` shards pass: every mutant of `rs/moq-net/src/model/resume.rs` and of `rekey` in `rs/moq-net/src/route/path.rs` fails a test, and a night's shards finish instead of being cancelled.

## Plan

[moq-dev/moq#9019](https://github.com/moq-dev/moq/pull/9019) killed the previous survivors; new ones followed later changes. Survivors seen:

- 11-16 ([43909101837](https://github.com/moq-dev/moq/actions/runs/43909101837)): 5 in `resume.rs` `Reader::window`, 1 in `route/path.rs` `rekey`.
- 11-17 ([44127032469](https://github.com/moq-dev/moq/actions/runs/44127032469)): all shards cancelled, after more missed in `resume.rs`: `Subscriber::holds`, `previous_start`, `skipped_below`, `largest_finished`, and the `Route::answering` guard.
- 11-18 ([44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698)): shards 2–5 failed; read them for the current list.

Find first why the 11-17 shards were cancelled. Then, for `Reader::window`, add unit tests where a copy's failover cap equals, falls just below, or falls just above a range's start or end, and assert the settled window; add direct tests for the other listed methods and `rekey`. Re-run with `cargo mutants -f <file>`. A mutant that changes no behaviour is excluded with its reason, not tested.

Public API: none. Wire: none.

## Related

- [Mutants of the other modelled code](/quest/m4/mutants-modelled.md) - widens the run to more files
