# [S] Report synchronous stateful browser codec requirements

## Goal

The existing synchronous browser compression discussion records MoQ's
small-frame, stateful use case and the limitations of worker-only or
one-shot helpers. Link the submitted evidence and upstream response here;
browser implementation and MoQ adoption are separate work.

## Plan

Decided with the maintainer on 2026-10-21: preserve synchronous compressed
writes now and accept the measured approximately 2.9 KB gzip encoder cost
in the consumer's static bundle. Pursue native support separately rather
than changing immediate read or close timing or adding a preparation API.

- Start with [synchronous compression and decompression](https://github.com/whatwg/compression/issues/86)
  and the [worker-only proposal](https://github.com/whatwg/compression/issues/38).
  The [explainer](https://github.com/whatwg/compression/blob/main/explainer.md)
  explicitly excludes synchronous compression. Treat this as an API request,
  not a browser defect, and avoid a duplicate issue.
- Explain the consumer: producer writes and immediately observable group
  state use the existing synchronous contract. Frames share a DEFLATE
  dictionary within a group; a one-shot operation per frame cannot replace
  the codec. An asynchronous flush alone does not preserve that contract.
- Document small-frame workloads and bounded input/output needs, including
  hostile input and early refusal of output beyond the declared frame size.
  Reuse the current codec's measured workloads rather than inventing a new
  performance threshold or prematurely specifying a browser API shape.
- Explain which operations need to work on the main thread. A worker-only
  API does not make main-thread producer calls synchronous. Propose any
  MoQ API or timing changes to the maintainer before pursuing them.
- Prepare a concrete contribution to the existing issue and request any
  external-post authorization required by repository instructions. Record
  the submitted link and actual upstream position without requiring a
  standards decision before this report can finish.

No public API, wire or new documentation page in this reporting work.

## Related

- [Frame flush](/quest/m2/native-compression-flush.md) - nonfinal output while retaining the dictionary
- [Native adoption](/quest/m2/native-compression-adopt.md) - use native operations after verifying the existing contract
