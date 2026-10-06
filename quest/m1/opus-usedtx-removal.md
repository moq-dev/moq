# [XS] Remove Opus DTX from @moq/publish

## Goal

`OpusConfig.usedtx` is gone from `@moq/publish`, and the demo's DTX checkbox
with it, so no publisher can turn on browser Opus DTX. Breaking for
`@moq/publish`.

## Plan

Decided 2026-10-06 after the measured no-go in
[#4902](https://github.com/moq-dev/moq/pull/4902): with DTX on, Chromium
stamps each Opus chunk as the first input's timestamp plus the samples it
emitted, so every suppressed frame pulls later chunks earlier (14 s of input
ended at 8.9 s). No WebCodecs output (duration, metadata, `flush()`, Ogg)
recovers the dropped span, and Chromium's decoder collapses timestamp gaps on
the listener too. Firefox keeps the timeline; Safari ignores `usedtx`.

- Remove `usedtx` from `OpusConfig` (`js/publish/src/audio/encoder.ts`) and
  its test, and the checkbox and `opusDtx` signal in `demo/web/src/publish.ts`
  and `publish.html`.
- Update `doc/lib/js` wherever it names the option.
- Silence suppression for voice, if a customer ever wants it, is new work:
  encode without DTX and skip silent frames with our own voice detection, or a
  WASM libopus encoder ([Opus backend](/quest/m2/audio-opus-backend.md)).

Public API: removes `OpusConfig.usedtx` from `@moq/publish`. Wire: none.
