# [M] A relay resets a live group `Evicted` mid-stream under a pinned pool

## Goal

A relay whose pool is held past its capacity (`cache.headroom = "100%"`) never resets a
group it serves with `Evicted` while that group is owed to a reader within its budget: the
frames the pool evicted come back on a flight, or the group ends `Old` when it is stale.

## Plan

Found on [moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422). In its 66-run relay
repro at 8fca407bd8 (`moq import ts` through `moq-relay` to `moq export fmp4`, 500 ms
`--max-delay`), one run's export saw its current group, mid-stream, fail
`Stream(Evicted)` from the relay, then skipped it. The release build carried no probes, so
the path is unnamed. The flight paths that PR fixed (a stale refusal read off the wire, the
end judged past a prefetch, a frame resumed mid-read) no longer reset groups `Evicted` in
its instrumented runs.

Next: rerun that PR's instrumented repro (`.tmp/repro/run3.sh` in its worktree, with probes
on `GroupServe`'s failure sites, `poll_boarding`, `Refly::poll_frame`, and `board`'s early
returns) until the reset recurs, name the branch that fails the group without a flight,
and fix it with a unit test that fails first.
