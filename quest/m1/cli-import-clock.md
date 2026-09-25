# [M] CLI imports publish on the broadcast clock

## Goal

`moq import` of fMP4, TS, and FLV publishes timestamps on the shared broadcast
clock, like native capture and `js/publish` already do, including source
restarts, late first frames, and real idle gaps. Today the imports publish
source PTS verbatim against a wall clock sampled at startup, so a TS feed with
a large starting PTS or a late first frame advertises the wrong wall time.

## Plan

Use `moq_mux::Clock` and `SourceMap` with the root catalog `clock`; this adds
no clock API or catalog field. Select each source's initial mapping once,
account for a delayed first frame, and translate source resets onto the same
monotonic clock while preserving real idle gaps. System-wall adjustments do not
retime a running broadcast or old archive records. Preserve allowed B-frame
ordering within a group.

- fMP4 is passthrough, so translation must rewrite `tfdt`.
- A muxed source needs one mapping for all of its tracks, since interleaved
  audio and video can step back further than `SourceMap::MAX_REORDER`.
- Keep conversion at the adapter boundary and refuse an unmappable source
  explicitly. Discontinuity markers signal the existing playhead contract;
  they do not replace the wall epoch.

CI fixtures drive the import path, not only the clock helper: simultaneous
A/V, a late first frame, a restart to zero, a restart after idle, and retained
archive playback. Update the import docs.

## Related

- [GStreamer clock](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - separate source adapter
