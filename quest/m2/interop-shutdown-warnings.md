# [S] A passing interop run stops publishers cleanly

## Goal

A passing `just test interop --all` prints neither of the
two warnings it prints today. No publisher is still running 10 s after it
was stopped, and no `track::Producer` is dropped without `finish()` or
`abort()` during the CLI FETCH round. Each warning is either fixed at its
cause or shown to be expected, in which case the harness stops printing it.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7701](https://github.com/moq-dev/moq/pull/7701)): milestone m2.

- Publisher linger: `stop_publisher` in `test/interop/interop.sh`
  (around lines 1138–1152) warns "publisher '$lang' was still running 10 s
  after it was stopped; killing it". #7701 saw it for Rust. The comment near
  `interop.sh:1077` says a finished import stays up for its linger
  (max-age), so an explicit max-age, such as media's 30 s, may outlast the
  10 s.
  Decide whether a stop should end the linger, or the harness should wait
  for it.
- Unfinished producer: `track::Producer dropped without finish() or
  abort()` (`rs/moq-net/src/model/track.rs`) during the CLI FETCH round.
  Find which producer the CLI or the relay drops without finishing it, and
  finish or abort it.
- Reproduce both from a passing local run with the producer warning set to
  log a backtrace. Then add an assertion that a passing run logs neither
  warning, so they can't return unnoticed.

Public API: none expected. Wire: none.

## Related

- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - red runs; this quest covers warnings in green ones
