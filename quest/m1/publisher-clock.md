# [L] Use the broadcast clock across media publishers

## Goal

Native video/audio capture and CLI imports publish timestamps against the
shared broadcast clock, including source and encoder restarts.
A live-only publisher exposes its clock without constructing an archive.

## Plan

Use `moq_mux::Clock` and `SourceMap` with the root catalog `clock`; `js/publish`
already advertises its `performance.now()` mapping. Native video maps the
device timeline at open and audio stamps arrival, both on `catalog.clock()`.
CLI imports (fMP4, TS, FLV) still publish source PTS verbatim against a wall
sampled at startup; fMP4 is passthrough, so a translation must rewrite `tfdt`,
and a muxed source needs one mapping for all its tracks, since interleaved
audio and video can step back further than `SourceMap::MAX_REORDER`.

Select each source's initial mapping once, account for delayed first frames,
and translate source resets onto the same monotonic clock while preserving real
idle gaps. Share the clock between
audio and video. System-wall adjustments do not retime a running broadcast or
old archive records. Preserve allowed B-frame ordering within a group.

Keep source-specific timestamp conversion at the adapter boundary. Refuse an
unmappable source explicitly. Discontinuity markers signal the existing
playhead contract; they do not replace the wall epoch.

Add CI fixtures for simultaneous A/V, late first frames, restart to zero,
restart after idle, system-wall adjustment, and retained archive playback.
The fixtures must exercise publisher integration rather than only the clock
helper. Update publisher and import docs; this quest adds no new clock API or
catalog representation. GStreamer's clock observation remains its own quest.

## Related

- [GStreamer clock](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - separate source adapter
