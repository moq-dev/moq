# [M] The moq-net track benches' empty-run and fill fixtures pass

## Goal

`cargo bench -p moq-net --all-features --bench track -- --test` runs every group to the end.
Three fixtures in `rs/moq-net/benches/track.rs` panic on `main` before measuring, and each stops
the groups after it in that target.

## Plan

Found while smoke-running the benches for [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529).
Each failure below reproduces identically on 85ab682bf4, the commit before that PR:

- `track_empty_run/stale/1/64`: `EmptyRun::drain` expects `run + 2` groups handed out, and gets
  2. The run's groups sit about four hours behind the newest under a one-hour live budget, so the
  read writes them off. Decide whether the fixture or its expectation is wrong; the doc comment says
  only the run's last group is written off.
- `track_fill/1r/64`: `the update reached the fill` sees a fill at priority 0 after every reader
  raised its priority to 1, and once that is skipped, `readers share each run's fill` counts 1 of 64.
- `track_range_filling/span_1/16`: not every reader drains `ranges * width` filled groups.

For each, decide whether the fixture or the code is wrong, and fix it at the cause. Check whether
Nightly's measured run already reports them.
