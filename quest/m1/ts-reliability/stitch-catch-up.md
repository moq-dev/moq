# [M] A stitched switch returns to --delay

## Goal

After `moq export ts --stitch` switches to a replacement publisher instance,
end-to-end latency returns to `--delay` once the replacement acquires,
instead of carrying the old program's tail as added latency.

## Plan

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6919](https://github.com/moq-dev/moq/pull/6919): drop the old
tail, not time-compress. The switch is already a TS program change, so the
skip costs nothing extra.

- Today `commit` in `rs/moq-mux/src/jitter.rs` starts the replacement past
  the old `horizon`, and the anchor only moves forward. The only catch-up is
  clock steering capped at about 30 ppm (`MAX_DRIFT`, `MAX_SLEW`), so the
  3.9 s, 3.6 s, and 7.5 s measured in #6919's run would take about a day
  and a half each to remove.
- Once the replacement acquires, cut the old program's queued tail and
  start the new program at `--delay`. Keep output monotonic across the
  program change.
- Test repeated switches: latency settles back to `--delay` after each,
  rather than accumulating.
- Also reproduce #6919's priority observation: a mid-stream
  `Subscriber::update` demotion of the old leg still showed priority 0 on
  the receive side. Fix it if update propagation is wrong; otherwise
  explain it in the PR.
- Update `doc/bin/cli.md` and `doc/setup/redundancy.md` inline if they
  describe switch latency.

Public API: none expected. Wire: none.
