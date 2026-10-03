# [S] GStreamer picks the broadcast wall epoch

## Goal

`moqsink` exposes one fixed wall epoch for the broadcast through the shared
Hang clock contract, so every pad's timestamps relate to UTC the same way.

## Plan

The PTS mapping already exists: `rs/moq-gst/src/sink/pad.rs` maps each
buffer PTS through its TIME segment into the shared running-time domain
(`rs/moq-gst/src/sink/timeline.rs`), and #4480 made rewinds drop frames. Only
the wall-epoch choice remains.

Choose the wall epoch once per broadcast. Prefer GstReferenceTimestampMeta
only when it names a recognized absolute clock domain; otherwise relate the
pipeline clock, base time, running time, and local SystemTime. An unidentified
reference clock is not UTC. Every pad uses the same epoch rather than sampling
its own. Do not define a GStreamer-specific catalog shape.

Decided in the 2026-09-30 audit: a restart is a new broadcast epoch, not a
forward re-anchor on the old clock (per remove-live and
[GStreamer and OBS](/quest/m1/broadcast-epoch/gst-obs.md)), and
[#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md)
handles the sink side.

Test recognized reference metadata, the deterministic local-clock fallback,
delayed first buffers, and multiple pads sharing one epoch.

## Closes

- [#3021](https://github.com/moq-dev/moq/issues/3021) - close this issue when the quest finishes

## Related

- [GStreamer and OBS](/quest/m1/broadcast-epoch/gst-obs.md) - a restarted pipeline publishes a new epoch
