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

### Progress

The draft holds the initial catalog until a complete PCR window, four seconds
of media without one (a source with no usable PCR), or EOF, and grades PCR
timing from the first interval. The hold applies to every subscriber of a
broadcast whose producer uses the `mpegts` catalog extension.

Decided (2026-10-11): drain to consumers. A short input's media must reach a
subscriber already waiting on the broadcast, and an import never reports
success while delivering zero media. A source that never completes a PCR
window publishes unpadded rather than nothing.

Open: the drain has no signal to wait on. A one-second import releases its
catalog only at EOF, and the subscriber's media SUBSCRIBE reaches the
publisher after it has left (`subscribe to the video: Dropped`). The ignored
`a_short_import_drains_its_media_to_a_waiting_subscriber` in
`rs/moq-cli/tests/import.rs` reproduces it; un-ignore it once the drain lands.
Demand does not span the catalog-to-media handoff: the catalog subscription
ends at its FIN, a relay releases its upstream at that FIN, and the media
request is a separate stream, so the publisher sees no demand in between.
Holding the catalog unfinished until media demand appears deadlocks a
catalog-only subscriber, which `import_delivers_the_catalog_finish_at_eof`
relies on. Do not add a sleep or extend a shutdown timeout to hide the race.
