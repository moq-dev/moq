# [M] Measure GStreamer ingest copies against pool retention

## Goal

GStreamer ingest uses the least costly safe buffer ownership strategy,
proven with both fast and slow consumers.

## Plan

[Sink::forward_buffer](/rs/moq-gst/src/sink/imp.rs) receives an owned
GStreamer buffer but maps and copies its full encoded payload into Bytes.
Egress already wraps Bytes in a GStreamer buffer. The ingest
copy could be avoidable, but retaining a mapped upstream buffer can exhaust a
finite producer pool or pin more memory than copying.

Add ingest benchmarks beyond the existing catalog target. Sweep streams ×
subscribers, payload size/cadence, and independently the number of upstream
pool buffers and reader retention. Measure copy bandwidth, allocations,
CPU/frame, delivery tail latency, peak memory and upstream stalls. Include
multi-memory/noncontiguous buffers, map failures, flush, cancellation and
teardown; compare identical delivered payloads and timestamps.

Evaluate owned mapped backing only if end-to-end results justify it and its
Send/lifetime properties hold. Do not claim a win from removing memcpy alone,
or introduce an unbounded pool to make the experiment pass. Retaining the
current copy with a benchmark explaining why is a complete outcome.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
