# JS publish and watch hot paths

## Goal

Measure and reduce browser decode retention, PCM copies, header allocations,
and stream-open overhead while preserving public APIs and ownership contracts.

## Plan

Every child requires the shared browser benchmark harness before execution.
Use the same fixtures and paired base/current measurements, record browser and
codec configuration, and verify delivered output so dropped work cannot appear
to be a speedup. Microbenchmarks may isolate costs in Bun; browser conclusions
require real WebTransport or WebCodecs as appropriate. Wire bounded correctness
coverage into CI, at least nightly, without imposing timing gates on shared
runners. A measured no-win completes a quest with retained evidence.

Each child can land independently once its required harness is available.
Keep internal implementation choices private. Datagram delivery, public frame
ownership changes, and payload headroom redesign are outside this line.

## Quests

- [Bound watch video decode](/quest/next/js-hotpath/watch-decode.md) - bound decoded retention without changing public frame ownership
- [Watch audio copies](/quest/next/js-hotpath/watch-audio.md) - reduce PCM copies while preserving ring semantics
- [IETF object properties](/quest/next/js-hotpath/ietf-object-encode.md) - remove temporary property-encoding streams
- [Coalesce stream writes](/quest/next/js-hotpath/write-coalesce.md) - combine headers while retaining separate payload writes
- [Publish audio grouping](/quest/next/js-hotpath/publish-audio.md) - compare bounded groups on every supported transport
- [Capture worklet storage](/quest/next/js-hotpath/capture-sab.md) - measure a bounded shared capture ring against message copies

## Related

- [Browser benchmarks](/quest/next/browser-benchmarks.md) - shared prerequisite
- [Reader buffering](/quest/next/stream-buffering.md) - receive assembly
- [CMAF copies](/quest/next/cmaf-copy-budget.md) - container samples
