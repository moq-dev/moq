# JS publish and watch hot paths

## Goal

Browser publish and watch drop extra GPU surfaces, PCM copies, per-frame
allocs, and stream opens. Each child names a Bun microbench or a real
WebTransport + WebCodecs measurement.
[Browser benchmarks](/quest/m2/browser-benchmarks.md) is the harness to
extend, not a second one.

## Quests

- [Bound watch video decode](/quest/m2/js-hotpath/watch-decode.md) - O(1) live `VideoFrame`s, not O(delay × fps)
- [Watch audio copies](/quest/m2/js-hotpath/watch-audio.md) - `copyTo` into the SAB; mute disconnects the worklet
- [IETF object header encode](/quest/m2/js-hotpath/ietf-object-encode.md) - no `WritableStream` per frame
- [Coalesce stream writes](/quest/m2/js-hotpath/write-coalesce.md) - one WebTransport write per object
- [Publish audio grouping](/quest/m2/js-hotpath/publish-audio.md) - stop opening one QUIC stream per Opus frame
- [Capture worklet SAB](/quest/m2/js-hotpath/capture-sab.md) - no structured-clone of PCM every quantum

## Related

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - the runner
- [Reader buffering](/quest/m2/stream-buffering.md) - JS Reader copies
- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - container samples
