# [S] TS import publishes its catalog once the mux rate is measured

## Goal

A TS→MoQ→TS export is constant-rate from its first packet. `moq import ts`
holds the catalog until its mux-rate meter settles, so `mpegts.muxRate` is
there from the start rather than appearing about 2 s in. A VBR source that
never settles is published without it once the window closes.

## Plan

Decided (2026-10-01). Found in [fixed-delay release](/quest/m1/tstd/delay.md)
(#4645): the importer's meter (`rs/moq-mux/src/container/ts/mux_rate.rs`)
publishes `mpegts.muxRate` only after a 2 s window agrees. So the export's
first ~2 s go out unpadded, and the harness skips them (`pcr-timing.py`
grades from the first null packet).

- Hold the whole catalog until the meter settles or its window closes,
  whichever is first: a one-time ~2 s start-up cost at ingest. Media written
  in the meantime is still published.
- A source whose rate never settles (VBR) publishes after the window without
  `muxRate`, and the export stays unpadded, as today.
- Remove the harness's "grade from the first null packet" skip, and the
  export's grace for a rate that arrives mid-stream, if nothing else needs
  them.
- Test: an imported CBR source's first catalog carries `muxRate`, and a VBR
  source's catalog arrives after the window without it. A TS→TS export passes
  `pcr-schedule` from its first PCR.

## Required

- [Fixed-delay release](/quest/m1/tstd/delay.md) - its export is what reads the rate from the start
