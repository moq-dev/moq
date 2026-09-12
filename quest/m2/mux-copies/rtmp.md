# [M] Measure and reduce RTMP message assembly copies

## Goal

Reduce measured redundant assembly of complete RTMP messages while preserving
chunk parsing, message delivery, public APIs, and the wire format.

## Plan

The chunk deserializer splits bytes from its input buffer and copies them into
`current_payload_data`. Assembly is necessary across split chunks; a complete
message already held contiguously may permit a shared payload instead. Measure
both cases without assuming the negotiated chunk size fits typical video frames.

Keep the optimization private. Preserve chunk-stream interleaving, extended
timestamps, negotiated chunk-size changes, partial network reads, malformed
input errors, and ordering. Bound retained backing-buffer memory and verify that
later input-buffer mutation cannot alter delivered message bytes.

Add Criterion cases for complete messages, split messages, and interleaved chunk
streams with representative audio/video sizes. Wire message/timestamp equality,
ownership, and chunk-boundary regressions into existing RTMP CI tests. Follow
the measurement and no-win completion rules in the
[questline](/quest/m2/mux-copies/README.md).

## Related

- [FLV tag bodies](/quest/m2/mux-copies/flv-rtmp.md) - independent container parsing copies
