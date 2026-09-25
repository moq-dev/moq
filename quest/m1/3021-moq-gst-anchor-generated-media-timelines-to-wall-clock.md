# [M] GStreamer preserves the broadcast PTS-to-wall clock

## Goal

`moqsink` maps every media pad onto one continuous broadcast clock, exposing
its fixed wall epoch through the shared Hang contract. Source restarts never
change the interpretation of media already published.

## Plan

Use the current broadcast-wide segment timeline and clock contract, not the
old per-rendition timeline or removed `set_wall` method. Map the buffer PTS
through its TIME segment into the broadcast's running-time domain before
publication, preserving valid within-group B-frame reordering.

Choose the wall epoch once. Prefer GstReferenceTimestampMeta only when it
names a recognized absolute clock domain; otherwise relate the pipeline clock,
base time, running time, and local SystemTime. Account for the mapped PTS when
computing PTS zero. An unidentified reference clock is not UTC. Every pad uses
the same mapping rather than independently sampling a new epoch.

Keep that mapping through flushes, encoder restarts, and source PTS resets.
Translate a restarted source forward on the existing clock, including idle
gaps; refuse a source that cannot be mapped consistently. Discontinuity
markers never change wall or retime retained records. Do not add per-record
anchors or define a GStreamer-specific catalog shape.

Test recognized reference metadata, deterministic local-clock fallback,
delayed first buffers, multiple pads, timescale conversion, numeric limits,
source restarts, idle gaps, and a system-clock adjustment. Existing timeline
records remain unchanged. Consume the prerequisite's catalog format; no new
transport TIMESTAMP/TIMESCALE semantics, synchronization protocol, or drift
correction is introduced here. Run the GStreamer CI and `interop --all` lanes.

## Closes

- [#3021](https://github.com/moq-dev/moq/issues/3021) - close this issue when the quest finishes
