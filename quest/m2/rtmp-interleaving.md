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

First reproduce this with deterministic regressions: pause A between complete
RTMP chunks, complete a single-chunk message on B, then finish A. Test B both
shorter and longer than A's already buffered data, exercising underflow and
payload mixing separately. Drain get_next_message with empty input until None;
assert exact payloads, completion order, stream IDs, types, timestamps, and no
residual message. Splitting within a chunk tests fragmented input separately;
it is not valid CSID interleaving. The current implementation must fail.

Keep partial-message state per chunk-stream ID while retaining one parser for
the incoming byte stream. Preserve per-ID header compression and timestamp
history, chunk-size changes between messages, partial reads, and completion
order. Validate lengths before subtraction or allocation; malformed input must
return an error rather than corrupting another stream's state.

Set explicit per-connection limits on active chunk-stream IDs and aggregate
retained assembly bytes. Chunk size and the 24-bit per-message length do not
bound the aggregate. Check budgets before allocating or extending state; reject
the input and release connection state on exhaustion using the existing error
path. Choose and document numeric internal limits against supported client
workloads before implementation is accepted. Test each limit independently,
including many incomplete IDs, and verify reclamation on completion, teardown,
and repeated ID reuse. Keep this correctness repair separate from shared-buffer or copy
optimizations. Add the regression and fragmented/header-format cases to normal
RTMP CI tests and run smoke-full for gateway interoperability.

## Related

- [RTMP copies](/quest/m2/mux-copies/rtmp.md) - optimization requires this correct baseline
