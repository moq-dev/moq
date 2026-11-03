# [M] Verify FETCH streaming and group tails on a quiet Chromium host

## Goal

Rerun the Chromium group-tail memory measurement and frame benches on a quiet
host against the pre-[moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411)
base, the landed chunked storage, and the FETCH streaming head. Record the
comparison in [moq-dev/moq#7721](https://github.com/moq-dev/moq/pull/7721), and
fix any small-frame browser regression it establishes.

## Plan

The implementation in [moq-dev/moq#7721](https://github.com/moq-dev/moq/pull/7721)
streams moq-transport FETCH and subscription fill objects through the existing
chunked group reader, without collecting a requested slice. Bounded FETCHes
answer before payloads finish; whole-group FETCHes wait for the final count
without retaining payloads beyond the ordinary group tail. An evicted answer
fails rather than pinning data for a slow reader. Lite FETCH cancellation watches
reader demand throughout the frame pump, including a stalled payload. A stopped
egress releases its chunk wait. Public API: none. Wire format: none.

The functional coverage includes a requested slice and an individual frame
larger than the tail, mid-frame source/request/output failures, coalesced lite
readers leaving mid-frame, and disposal of a stopped egress's chunk listener.
`js/net/bench/fetch.ts` sweeps retained groups, independent readers, and small or
chunked frames; Nightly runs it alongside `js/net/bench/frames.ts`.

Decided 2026-10-31 (user): do not buffer moq-transport FETCH slices; cancel a
lite FETCH promptly when its last reader leaves mid-frame. The requested quiet
comparison remains unfinished. This implementation ran alongside the user's
`/quest-spawn m0/m1` work, so shared-host timings cannot establish the browser
performance outcome. Do not substitute them for the quiet-host comparison.

## Waiting

- Maintainer: provide a quiet host or a reserved measurement window for the
  Chromium group-tail memory and paired frame benches. Recommend comparing the
  pre-7411 base, landed chunked storage, and FETCH streaming head there, then
  fixing any demonstrated small-frame regression before completing this quest.
