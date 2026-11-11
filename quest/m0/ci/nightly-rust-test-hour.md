# [M] Nightly's Rust benchmark tests fit their hour

## Goal

`Nightly (Rust benchmark tests)` (`just gh nightly rust-test`) finishes well
inside its 60-minute `timeout-minutes` on a scheduled run, with every benchmark
it runs today still measured in a release build, one at a time, and reported.
Splitting it across jobs and cutting what it builds are both in scope.

## Plan

Decided 2026-11-10 (maintainer, /quest-unblock): ✅ split the job and reduce
its size (rejected: finish the hakari measurement with one run; raise the
timeout).

Evidence: the job was cancelled at the timeout on the 2026-11-03, 11-05,
11-07, 11-08, 11-09, and 11-10 scheduled runs; the 11-04 and 11-06 runs
finished in 57 to 59 minutes, and the last to pass, on 11-02, took 34. In
[the 11-10 run](https://github.com/moq-dev/moq/actions/runs/42842827718/job/129922278502)
the benchmarks themselves ran for about two minutes in all. The hour went to
`sh/gh/nightly.sh`'s `rust-test` group compiling a separate release build per
`nextest` invocation (3 to 5.5 minutes each, about ten of them), and the job
was still compiling `moq-ffi` when it was cancelled.

Start from the builds: fewer `nextest` invocations sharing one release build
where the feature sets allow it, then split what remains across matrix entries
so no entry nears the hour. Keep `sh/gh/nightly.test.py`'s guarantees (release
builds, one JUnit report per run, the report publish step) or update them with
the change.

## Related

- [hakari follow-ups](/quest/m0/ci/hakari-followups.md) - its mbx measurement needs a complete run of this job
- [Relay session benchmarks fit their hour](/quest/m0/ci/nightly-relay-session-hour.md) - the same timeout in another Nightly job
