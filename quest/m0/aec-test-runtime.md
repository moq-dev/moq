# [S] Keep the echo-delay regression within the test budget

## Goal

`moq-audio`'s `aec::tests::survives_a_moving_echo_delay` finishes comfortably
within the normal nextest timeout in the workspace suite, retaining coverage
of a filter growing, shrinking, and reconverging as the echo delay changes.

## Plan

During local macOS/Nix validation of PR #3458, the unchanged test exceeded
60 seconds in the full scoped suite with both default concurrency and
`NEXTEST_TEST_THREADS=2`. An isolated
`nix develop --command just rs test -p moq-audio survives_a_moving_echo_delay`
passed in 27.241 seconds. The full-run timeout cancelled the remaining tests.
The mechanism is not yet established; an isolated pass does not explain the
workspace failures. A synchronous JS pattern test on the same host also
exceeded its five-second budget: 7.02 seconds wall time versus 0.87 seconds
user CPU and 0.18 seconds system CPU. Use that as a control when separating
host contention from DSP cost.

Reproduce with the workspace's feature graph and the isolated crate's graph,
recording CPU time, wall time, compiler profile, and concurrent build load.
Check whether unoptimized Sonora DSP dominates the bounded simulation. If
so, optimize the appropriate dependency in the dev/test profile, following
the existing bignum overrides, rather than weakening the changing-delay
regression. Otherwise fix the measured bottleneck at its owning layer.

Keep the normal timeout and full convergence assertion. Validate the fix in
the full suite and under representative concurrent development load, and
confirm that the upstream adaptive-filter shrink regression remains covered.

## Related

- [Worktree QA isolation](/quest/m0/worktree-qa-isolation.md) - make concurrent validation resources explicit
- [PR #3458](https://github.com/moq-dev/moq/pull/3458) - validation that exposed the timeout
