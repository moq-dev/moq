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

Facts already established, so the measurement starts from them. The input is
fixed run to run: `Room` is driven by the xorshift `Noise`, so whatever varies
is the host or the profile, never the workload. That workload is 1500 calls to
`Room::round`: 400 to grow the filter past its starting size, five 200-round
moves that grow and shrink it again, and 100 more to measure re-convergence.
Those counts are round numbers rather than measured minimums, so if a profile
override does not close the gap, the next thing to measure is the shortest
schedule that still reaches the shrink, before anything touches the assertions.
`aec` is a default feature, so this runs on the PR path for every change to
`moq-audio`, and it was hit again during PR #3466's session from two worktrees
building concurrently on the same host.

What the test protects is why weakening it is the last resort: the shrink path
panicked in sonora 0.1.0, which is why the crate pins `sonora = "0.2"`, and the
workspace's `panic = "abort"` makes that fatal in a real binary rather than a
caught unwind.

## Related

- [Worktree QA isolation](/quest/m0/worktree-qa-isolation.md) - make concurrent validation resources explicit
- [PR #3458](https://github.com/moq-dev/moq/pull/3458) - validation that exposed the timeout
