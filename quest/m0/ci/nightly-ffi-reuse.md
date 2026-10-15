# [XS] Nightly after FFI reuse

## Goal

Condition: a scheduled Nightly and the first `moq-ffi-v*` tag have completed
since [#5549](https://github.com/moq-dev/moq/pull/5549) (shared FFI build)
and [#5527](https://github.com/moq-dev/moq/pull/5527) (independent Nightly
workloads) landed. Check with `gh run list --workflow nightly.yml --event schedule`
and the FFI release runs for the tag. Then measure and verify the items
below, fix or quest what fails, and delete this quest.

## Plan

From the Nightly run:

- Duration of each workload job and total runner-minutes, against the
  sampled numbers in `.github/ffi.md`.
- Every benchmark still runs, including the timebase and audio benchmarks
  that landed beside the split.
- No workload fails, and the Rust group fits its existing limit.
- Size-history keys are stable across the change.

From the first `moq-ffi-v*` release:

- The nested coordinator job names match what the publisher check looks up.
- The artifact patterns select the right libraries for each package.
- The Swift wrapper dry run passes under the release's permissions.

Don't dispatch Nightly on a branch to get these; wait for the scheduled run.
