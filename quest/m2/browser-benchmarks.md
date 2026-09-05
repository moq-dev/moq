# [M] Browser transport and media performance benchmarks

## Goal

A reproducible browser suite measures JS transport and media costs that the Rust
Criterion targets and native relay load generator do not exercise.

## Plan

`rs/scripts/bench.sh::criterion_targets` discovers Cargo targets only. Existing JS unit
tests validate behavior, and `test/wasm` validates browser interop, but neither
provides a repeatable JS performance comparison. Reuse the existing relay/browser
harness pieces and add a focused recipe with artifacts under the benchmark
conventions. Microbenchmarks may run in Bun; browser conclusions must come from
an identified browser version on a real WebTransport connection.

- Cover `js/net/src/stream.ts` with buffered controls, fragmented varints, and
  payloads from small audio through large keyframes. Sweep chunk sizes and record
  CPU, wall time, allocation volume, GC pauses, and bytes copied where measurable.
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

- [Reader buffering](/quest/m2/stream-buffering.md) - fragmented receive workloads
- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - container workload and ownership checks
- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - reporting conventions
