# [L] Use the broadcast clock across media publishers

## Goal

Native video/audio capture, CLI imports, and `js/publish` publish timestamps
against the shared broadcast clock, including source and encoder restarts.
A live-only publisher exposes its clock without constructing an archive.

## Plan

Use the dev clock owner and catalog schema. Select each source's initial mapping
once, account for delayed first frames, and translate source resets onto the
same monotonic clock while preserving real idle gaps. Share the clock between
audio and video. System-wall adjustments do not retime a running broadcast or
old archive records. Preserve allowed B-frame ordering within a group.

Keep source-specific timestamp conversion at the adapter boundary. Refuse an
unmappable source explicitly. Discontinuity markers signal the existing
playhead contract; they do not replace the wall epoch. Coordinate browser
encoder restart markers with the discontinuity prerequisite.

Add CI fixtures for simultaneous A/V, late first frames, restart to zero,
restart after idle, system-wall adjustment, and retained archive playback.
The fixtures must exercise publisher integration rather than only the clock
helper. Update publisher and import docs; this quest adds no new clock API or
catalog representation. GStreamer's clock observation remains its own quest.

## Required

- [Publisher discontinuity](/quest/next/js-publish-discontinuity.md) - use the settled browser restart-marker path

## Related

- [GStreamer clock](/quest/next/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - separate source adapter
