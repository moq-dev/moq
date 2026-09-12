# [M] Reassemble interleaved RTMP chunk streams independently

## Goal

Interleaved RTMP chunk streams produce the same complete messages and timestamps
as each stream decoded independently, without mixing payloads or underflowing
remaining message lengths. Preserve the public API and wire format.

## Plan

`ChunkDeserializer` keeps previous headers per chunk-stream ID but has one
`current_payload_data` and one current message for all IDs. An incomplete
message on A followed by a chunk on B therefore shares assembly bytes, and
B's remaining length is computed against A's buffered payload.

First reproduce this with a deterministic regression: split a message on A,
complete a shorter message on B, then finish A. Assert exact payloads, stream
IDs, types, and timestamps, including the case where B is shorter than A's
already buffered data. The current implementation must fail the test.

Keep partial-message state per chunk-stream ID while retaining one parser for
the incoming byte stream. Preserve per-ID header compression and timestamp
history, chunk-size changes between messages, partial reads, and completion
order. Validate lengths before subtraction or allocation; malformed input must
return an error rather than corrupting another stream's state.

Bound and reclaim incomplete-message state under the existing input limits;
cover completion and teardown and verify that repeated ID reuse does not retain
old payloads. Keep this correctness repair separate from shared-buffer or copy
optimizations. Add the regression and fragmented/header-format cases to normal
RTMP CI tests and run smoke-full for gateway interoperability.

## Related

- [RTMP copies](/quest/m2/mux-copies/rtmp.md) - optimization requires this correct baseline
