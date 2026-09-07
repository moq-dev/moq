# [S] js/watch: a catalog republish rebuilds the audio graph only when the decoder identity changes

## Goal

A republished catalog whose audio rendition changed `bitrate`, `jitter`,
`label`, or any other field that does not affect decoding leaves the
AudioContext, worklet, and ring in place. Only a change to what the decoder
needs (codec, container, description, sample rate, channel count) rebuilds
them, the way video already works. An advertised `jitter` of 0 is treated as
absent and falls back to the codec's frame duration, since a zero flush delay
is not physically meaningful.

## Plan

The files are identical on main and dev; branch from main.

- `js/watch/src/audio/decoder.ts` `#runWorklet` subscribes to the whole
  `source.out.config` signal and rebuilds the context on any field change.
  `@moq/signals` deep-compares, so a byte-identical republish is fine, but the
  TS importer refines `bitrate` five times in the first minute and the fMP4
  importer lowers `jitter`, each tearing the graph down with a ring reset and
  a late-frame burst.
- Video solved this in #2865: `js/watch/src/video/config.ts` `playbackIdentity`
  picks the decoder-relevant fields and `video/decoder.ts` keys its pipeline
  on that. Add the audio equivalent (`audio/config.ts`) with `codec`,
  `container`, `description`, `sampleRate`, `numberOfChannels`, and key both
  `#runWorklet` and `#runDecoder` on it. `jitter` stays outside the identity:
  `#runLatency` already applies a depth change through `ring.setLatency`
  without a rebuild.
- `js/watch/src/audio/source.ts` `codecJitter = config.jitter ??
  defaultAudioJitter(config)` honors 0. Treat 0 as absent.
- Tests mirror `video/config.test.ts`: which fields may rebuild, a
  bitrate-only republish creates one worklet, and a `jitter: 0` catalog
  resolves the codec floor.

## Related

- [Jitter estimator](/quest/m0/3479-mux-jitter-flush-span.md) - the publisher half, and the issue this closes
- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - the ring these rebuilds reset
