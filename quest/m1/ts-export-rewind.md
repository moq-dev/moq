# [S] TS export restarts its clock only for a new timeline

## Goal

`moq export ts` keeps its program clock running through gaps and sets the PCR
`discontinuity_indicator` only when the clock really restarts: a `--linger`
resume or a replaced broadcast. One resume flags once. A timestamp that goes
backwards within one broadcast fails the export.

## Plan

`rs/moq-mux/src/container/ts/export.rs` emits the PCR on a fixed media-time
grid and fills every slot a gap crosses, so a forward gap already keeps the
clock continuous. But it calls `rewind()`, restarting the whole program clock
and setting the flag, whenever any track's consumer generation changes, and
the consumer bumps its generation on a latency skip or gap walk too. Catching
up on a resume backlog therefore rewinds up to four extra times.

Decided (2026-10-04):

- A forward gap on any track, however long, is filled on the PCR grid; the
  clock keeps running and nothing is flagged. With `mux_rate` padding the
  export already emits null packets through quiet periods.
- Restart the clock and set the flag only on a resume or a replaced
  broadcast.
- Time going backwards within one broadcast is a publisher bug (a name always
  means the same content), so the export fails with an error rather than
  rewinding.
- No new public API: the generation-change handling lives behind the
  crate-private `ExportSource`.
- [Fixed-delay release](/quest/m1/tstd/delay.md) rewrites the same release
  path and re-anchors on each rewind; land this inside or after it.

Tests: a resume that walks gaps on two tracks sets the indicator once; a 10 s
forward gap on one track keeps the PCR continuous with no flag; a backwards
timestamp fails the export.

## Closes

- [#4767](https://github.com/moq-dev/moq/issues/4767) - close this issue when the quest finishes

## Related

- [Fixed-delay release](/quest/m1/tstd/delay.md) - re-anchors the release clock on each rewind
- [TS hitless](/quest/m2/ts-hitless.md) - discontinuity flags across a source switch
