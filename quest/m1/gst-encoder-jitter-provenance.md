# [M] moq-gst: observe flush jitter only for local encoders

## Goal

A `moqsink` pad fed by a local encoder measures catalog jitter from the frame's
transport handoff, while file, pipe, demuxed, and network imports remain
clock-free. A GStreamer TIME segment and PTS alone cannot identify provenance:
`multifilesrc ! parsebin ! moqsink` supplies both.

## Plan

- Choose an explicit opt-in on the request pad (name and lifecycle to settle
  with the maintainer). Default to imports, which retain batch/reorder estimates.
- For an opted-in pad, call the explicit codec importer flush observation after
  a successful media write, with the mapped broadcast PTS and `Instant::now()`.
  Refuse an invalid opt-in/timestamp combination instead of silently skipping.
- Exercise a local encoder pipeline and the existing looped MP4 import recipe;
  prove only the opted-in path raises jitter. Document the opt-in in
  `doc/bin/gstreamer.md` and update demo pipelines where they encode locally.

## Related

- [Jitter clock](/quest/m1/jitter-flush-clock.md) - the flush measurement this pad feeds
