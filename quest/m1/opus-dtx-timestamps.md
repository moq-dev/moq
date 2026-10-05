# [M] Opus DTX keeps the capture timeline

## Goal

Opus DTX is correct in `@moq/publish` and on again by default for voice:
every encoded chunk is published at its input's capture time even when the
encoder suppresses silent frames, and a listener plays it on that timeline, so
voice saves bandwidth during silence without skewing jitter estimates.

## Plan

Chromium stamps encoder output as the first input timestamp plus the samples
it emitted, so suppressed frames pull later chunks earlier. DTX is off by
default but still opt-in through `OpusConfig.usedtx` and the demo's checkbox,
both of which drift today. Once the timeline holds, restore `usedtx: true` in
the voice defaults (`opusKindDefaults`).

Decided (2026-10-04):

- Research first: whether WebCodecs exposes enough to map an output chunk
  back to its input frame (chunk duration, a per-frame input queue, chunk
  metadata) in Chromium, Firefox, and Safari.
- The listener is in scope: `js/watch/src/audio/decoder.ts` inserts audio at
  the AudioDecoder's output timestamp, and if that also counts samples, a
  correct publisher still collapses at playback.
- A measured no-go deletes this quest and DTX stays off. Record why in the
  deleting PR.

Open for the research: whether silence is absent frames, as WebRTC sends, or
the empty DTX frame every 20 ms that Rust publishes today. Absent frames save
groups as well as bytes.

## Related

- [Opus backend](/quest/m2/audio-opus-backend.md) - DTX on the native side
