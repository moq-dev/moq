# [M] A timebase step restarts the TS import under a new epoch

## Goal

`moq import ts` and `moq import srt` keep a broadcast available across a
source timebase step. A rewind, flagged or not, and an unflagged step in
either direction end the importer's instance. The CLI then republishes in the
same process as a new broadcast under a fresh epoch, instead of exiting.
Subscribers follow the new instance like any republish, and no `moq export ts`
exits on a step it reads as clock drift.

## Plan

Root cause of [#7102](https://github.com/moq-dev/moq/issues/7102), established
2026-10-28: the contribution comes from two redundant encoders behind a
changeover switch. Their clocks share a frequency reference (the offset holds
at ~16,844.5 s, a few ppm over days) but start ~4 h 40 m apart, and the switch
sets no `discontinuity_indicator` (ISO/IEC 13818-1 §2.4.3.5, TR 101 290 2.3b).
That is an upstream misconfiguration: redundant encoders are normally
time-locked, or the switch flags the break. MoQ tolerates it anyway, as ffmpeg,
GStreamer and VLC do by inferring a break from the jump.

Decided 2026-10-28 (maintainer):

- **Two encoders are two broadcasts.** An unflagged step means a different
  source, so it gets a new epoch rather than a splice. This keeps #4543's rule
  (group starts never go backwards within an instance) and the m1 decision
  that a redundant ingest pair runs on distinct epochs. A flagged forward step
  still splices with break markers, as today.
- **Detection:** PCR and every timed PID step together, forward beyond ~10 s
  or backward by any amount that is not a 33-bit wrap. These are ffmpeg's
  `dts_delta_threshold` bounds, not TR 101 290's 100 ms, which SRT loss can
  exceed. `ts/health.rs` already computes the modular step for 2.3b; reuse it.
- **The supervisor lives in the CLI, not moq-mux.** The importer ends its
  instance with a typed restart cause. `moq import ts/srt` republishes under a
  fresh epoch on the same path, keeping the SRT connection. This is the
  supervisor the 2026-10-08 decision allows ("a supervisor that wants to
  continue publishes a new broadcast under a new epoch"). An in-process
  `import.restart(broadcast)` in moq-mux stays rejected.
- Rejected: inferring the flag forward (a splice across two sources), and
  reopening #4543 with a catalog timeline generation (a second epoch under
  another name, conflicting with time addressing).

Tests: a synthetic clip with the whole-multiplex unflagged step both ways (the
reporter offered a public reproducer) restarts under a new epoch. A
subscriber's `moq export ts --stitch` follows it. Lossy PCR gaps under the
bound never restart.

Docs: `doc/bin/cli.md` ("run it again to publish anew") describes the restart
and names the upstream fix (time-locked encoders, or a flagged switch).

Public API: the importer's error gains a restart cause. Wire: none.

## Closes

- [#7102](https://github.com/moq-dev/moq/issues/7102) - an unflagged timebase step ends `moq import` and `moq export ts`

## Related

- [A restart keeps its catalog](/quest/m1/ts-reliability/restart-catalog.md) - removes the catalog hold a restart pays
- [Decide whether a stitched export demotes its old leg](/quest/m1/ts-reliability/stitch-catch-up.md) - may speed the replacement's first keyframe at each restart's switch
