# [S] TS import publishes its catalog once the mux rate is measured

## Goal

A TS→MoQ→TS export is constant-rate from its first packet. `moq import ts`
holds the catalog until its mux-rate meter settles, so `mpegts.muxRate` is
there from the start rather than appearing about 2 s in. A VBR source that
never settles is published without it once the window closes.

## Plan

Decided (2026-10-01). Found in the fixed-delay release
(#4645): the importer's meter (`rs/moq-mux/src/container/ts/mux_rate.rs`)
publishes `mpegts.muxRate` only after a 2 s window agrees. So the export's
first ~2 s go out unpadded, and the harness skips them (`pcr-timing.py`
grades from the first null packet).

- Hold the whole catalog until the meter settles, or until one `WINDOW` of
  PCR time has passed with no rate published, whichever is first: a
  one-time ~2 s start-up cost at ingest. The meter itself keeps rolling and
  never times out, so the hold needs that explicit bound. Media written in
  the meantime is still published.
- A source whose rate never settles (VBR) publishes after that window
  without `muxRate`, and the export stays unpadded, as today.
- End of input releases a held catalog without `muxRate` before
  `catalog.finish` closes it: a TS shorter than one window never completes
  a measurement, and finishing would otherwise close the catalog track with
  the reservation unpublished, leaving media with no catalog.
- Remove the harness's "grade from the first null packet" skip, and the
  export's grace for a rate that arrives mid-stream, if nothing else needs
  them.
- Test: an imported CBR source's first catalog carries `muxRate`, a VBR
  source's catalog arrives after the window without it, and a CBR import
  shorter than 2 s still publishes its catalog. A TS→TS export passes
  `pcr-schedule` from its first PCR.
