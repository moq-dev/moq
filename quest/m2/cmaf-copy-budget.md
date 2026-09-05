# [M] Measure and reduce JS CMAF payload copies

## Goal

Establish the allocation/copy budget of CMAF encode/decode and remove redundant
work where it measurably improves media throughput without changing bytes or
sample-buffer ownership.

## Plan

`js/hang/src/container/cmaf/encode.ts::encodeDataSegment` serializes the moof twice
to resolve `trun.dataOffset`, copies the payload into a fresh mdat buffer, then
concatenates library output into the final segment. In
`decode.ts::decodeDataSegment`, `new Uint8Array(mdatData.slice(...))` can copy a
sample twice when the parser returns an ordinary Uint8Array. Confirm the parser's
actual runtime types and ownership before treating that expression as removable.
`toArrayBuffer` also materializes parser input. These are observed operations;
their share of end-to-end playback cost is not yet measured.

- Use fixed Opus/AAC and H.264/AV1 fixtures at several sample sizes, including large
  keyframes and multi-sample fragments. Measure encode/decode separately, recording
  payload bytes copied, allocation count/volume, CPU, and GC time in Bun and a
  supported browser. Keep fixture construction outside timing.
- Inspect the locked ISO-BMFF library's accepted buffer types, write ownership,
  and parser lifetimes. Prefer its maintained buffer/scatter facilities over a new
  handwritten container parser. Compare eliminating duplicate copies first;
  separately evaluate serializing headers once without assuming a fixed moof size.
- Preserve independent writable sample results and support subarrays with nonzero
  offsets. Returning views instead of copies may pin whole segments or allow one
  caller's mutation to affect another, so measure retained memory as well as churn.
- Validate decoded payload identity, sample order, timescale conversion, signed
  composition offsets, flags, durations, and invalid-input handling. Roundtrip
  against existing fixtures and another parser where available; verify encoded
  wire bytes remain equivalent before calling this a pure implementation change.
- Land only measured improvements with a regression for the removed redundant
  work and ownership tests that retain/mutate outputs across later calls. Record
  no-win results rather than replacing library code on intuition alone.

## Related

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - container and playback measurement
- [Reader buffering](/quest/m2/stream-buffering.md) - upstream payload assembly costs
