# [S] TS export rewinds only on a real timebase break

## Goal

`moq export ts` sets the PCR `discontinuity_indicator` once per real timebase
break: a `--linger` resume, a declared discontinuity marker, or a replaced
broadcast. A latency skip or gap walk on one track drops that track's frames
without restarting the programme clock.

## Plan

`rs/moq-mux/src/container/ts/export.rs` calls `rewind()` whenever any track's
consumer generation changes. The consumer bumps its generation on a latency
skip or gap walk too, so catching up on a resume backlog rewinds the whole
programme up to four extra times.

Decided (2026-10-04): the consumer reports why its generation moved (marker vs
latency skip), and the TS export rewinds only on a marker, on top of the
rewind `resume()` already does. A skip forward on one PID needs no PCR reset.

Coordinate with [fixed-delay release](/quest/m1/tstd/delay.md), which rewrites
the same export's release path; whichever lands second rebases.

Tests: a resume that walks gaps on two tracks sets the indicator once; an
explicit marker still sets it.

## Closes

- [#4767](https://github.com/moq-dev/moq/issues/4767) - close this issue when the quest finishes

## Related

- [Fixed-delay release](/quest/m1/tstd/delay.md) - re-anchors the release clock on each rewind
- [TS hitless](/quest/m2/ts-hitless.md) - discontinuity flags across a source switch
