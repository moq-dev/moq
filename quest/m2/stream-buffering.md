# [M] Bound JS reader copy cost under fragmented input

## Goal

Measure and, if justified, make fragmented payload assembly linear in payload
size without changing decoded bytes, error behavior, or returned-buffer ownership.

## Plan

`js/net/src/stream.ts::Reader.#fill` allocates a combined buffer and copies the
entire unread prefix whenever another chunk arrives. `#fillTo` and `readAll`
repeat this operation. For N equal chunks retained until one read completes,
coalescing copies grow with N squared. Removing an intermediate incoming-chunk
copy reduces constants but leaves that mechanism intact. This is source evidence,
not a measured browser bottleneck yet.

- Benchmark the current Reader with fixed total payloads (1 KiB, 64 KiB, 1 MiB,
  and a bounded large keyframe), split into 1-byte, 1 KiB, 16 KiB, and whole-payload
  chunks. Bound the pathological baseline so the experiment itself cannot exhaust
  memory. Include streaming small reads, unread leftovers, and `readAll`.
- Count payload allocations/copied bytes separately from elapsed time and GC.
  Compare a chunk queue with one final copy against geometric capacity growth.
  Account for retained backing buffers: a small returned view can pin a large
  arena, and growing/reusing storage must not mutate earlier returned results.
- Preserve isolation from caller-owned streamed chunks, nonzero byte offsets,
  EOF/reset propagation, size limits, and exact cross-chunk varint behavior.
  Test input mutation after reads and retain results across subsequent fills.
- Evaluate BYOB only as a separately measured option. The Reader's comment claims
  WebTransport workers cannot use it, while the current WebTransport specification
  exposes readable byte streams. Verify actual browser/worker support and fallback
  transports before changing that assumption; spec support is not runtime proof.
- Keep the async API unchanged. Coordinate buffer/cursor changes with the existing
  synchronous-decoder quest, which owns control-first ordering and queue removal.
- Accept an implementation only with linear copy/allocation evidence on the
  fragmented path and no material regression for already-buffered/single-chunk
  reads. Commit a bounded regression that fails when repeated prefix copying
  returns, plus browser before/after results or a documented no-win conclusion.

Reference: [WebTransport receive streams and BYOB](https://www.w3.org/TR/webtransport/).

## Related

- [Synchronous decode](/quest/m2/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - owns decoder and control ordering changes
- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - reusable measurement harness
