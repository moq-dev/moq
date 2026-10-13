# [M] Browser transport and media performance benchmarks

## Goal

A reproducible real-browser suite measures JS transport and media costs that
the Rust Criterion targets, the native relay load generator, and the Bun
microbenchmarks do not exercise.

## Plan

Decided 2026-10-12: keep this broader transport/media suite in m2. The
[backend baseline](/quest/m1/rs2ts/spike.md) adds the focused browser harness
in m1, and each backend adoption includes its own comparison before switching.
Reuse that work here; backend adoption does not wait for this broader suite.

The JS microbenchmarks already exist: the seven Bun sweeps in `js/net/bench`
(`broadcasts`, `forward`, `frames`, `lite-varint`, `reader`, `track`,
`varint`) run nightly and cover the origin map, forwarded route re-pricing,
group frame decode, lite-07 against lite-06 varints, fragmented `Reader`
reads, track retention, and varint coding. This
quest is only the real-browser half: conclusions come from an identified
browser version on a real WebTransport connection. `test/wasm` validates
browser interop but measures nothing. Reuse the existing relay/browser harness
pieces and add a focused recipe with artifacts under the benchmark conventions.

- Measure the `js/net/src/stream.ts` path in the browser over WebTransport,
  with payloads from small audio through large keyframes, recording CPU, wall
  time, allocation volume, GC pauses, and bytes copied where measurable. Add a
  Bun sweep only for a cost the browser run finds and the seven miss.
- Cover CMAF encode/decode with fixed audio/video fixtures and multiple samples.
  Keep fixture generation and relay startup outside timed intervals.
- Add publish/watch scenarios measuring delivered/decoded/presented frames,
  dropped frames, decode queue depth, long tasks, heap trend, and tail frame delay.
  Fix codec, resolution, framerate, device, visibility, and hardware acceleration;
  a hidden tab or hardware decode change invalidates a comparison.
- Run warmup and repeated alternating base/current samples. Separate network,
  decode, and render costs; report unsupported metrics as unavailable. Include
  payload checksums/counts so skipping work cannot look like a speedup.
- Retain browser traces and environment metadata. Keep timing opt-in initially;
  a bounded smoke lane should fail for crashes, missing output, or invalid samples,
  not for an arbitrary percentage slowdown on a shared CI runner.
- Demonstrate A/A variability and detect an injected copy-heavy variant without
  conflating instrumentation overhead with normal playback cost.

## Related

- [Browser media report](/quest/m2/rs2ts-browser-report.md) - compares Rust-backed and handwritten @moq/net through this broader harness
- [Benchmark reporting](/bench/README.md) - reporting conventions
