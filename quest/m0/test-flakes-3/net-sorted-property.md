# [S] @moq/net sorted-index property test times out under load

## Goal

`js/net/src/util/sorted.test.ts`'s `agrees with a sorted array on every lookup
after any inserts and deletes` passes every loaded `just check`, fixed at its
cause.

## Plan

Found on [moq-dev/moq#9521](https://github.com/moq-dev/moq/pull/9521), which
doesn't touch `util/sorted.ts`: it failed at 7.4 s under a full `just check` and
took 0.28 s alone. Each of its 100 fast-check runs replays up to 4000 operations
at `size: "max"` and then rescans the model array for every probe, so the
model's linear scans likely dominate, not `Sorted`.

Profile one run first. Recommended: keep the coverage (chunks of 256 split,
merge, and empty) with fewer, cheaper runs, such as a binary search over the
sorted model instead of `find`/`filter` per probe. Don't raise the timeout.
