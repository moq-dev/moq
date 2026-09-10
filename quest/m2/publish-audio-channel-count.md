# [M] Forcing a channel count on Audio.Capture does not cost audio

## Goal

`new Audio.Capture({ source, channelCount: 1 })` publishes continuous audio.
A subscriber hears no gaps that the same capture without the override does not
also have, so the documented way to pin a publisher's channel count stops being
a way to degrade it.

Boundaries: `channelCount` keeps its meaning, "force the captured channel
count", and the published shape does not move. Whether the fix is to stop
forcing `channelCountMode: "explicit"`, to do the downmix off the realtime
graph, or to keep the mode and remove whatever it starves, is the quest's to
decide.

## Plan

`js/publish/src/audio/capture.ts:178` sets
`channelCountMode: requestedChannels !== undefined ? "explicit" : "max"`, so
supplying `channelCount` is what puts the AudioWorklet behind a forced Web
Audio downmix. The `"max"` path, taken when the field is omitted, does not.

What was measured, on macOS headless Chromium through `just test smoke-media`,
publishing a `MediaStreamAudioDestinationNode` track into a graph already
running at 48 kHz:

- With `sampleRate: 48000, channelCount: 1`: 11 probe samples across a run read
  a flat spectrum (peak equal to median, below `AUDIBLE_RMS`), which is
  silence, not a weak tone. The `audio tone` check fell to 82-88% against its
  90% floor and took two negative controls down with it.
- With both overrides removed: zero such samples, and every media check passed.
- `main`, whose fixture pinned the same format on the encoder rather than the
  capture, reported `tone 150dB above the floor` and no silent samples at all.

What is not established, and should be first:

- The two overrides were removed together, so `channelCount` is implicated by
  reading line 178, not by an isolated run. Re-run with `sampleRate` alone and
  with `channelCount` alone before believing the attribution.
- The mechanism. A forced downmix starving or stalling the worklet is the
  obvious guess and is not evidence.
- Whether a `getUserMedia` microphone track shows it, or only a
  destination-node track, whose channel count Web Audio reports as 2 by
  default. `requestedChannelCount` (`capture.ts:141`) exists for the macOS
  mono-mic misreport, so that path has a real caller and cannot simply lose the
  override.

The regression belongs where the evidence came from: the browser fixture can
pin a channel count again once this is fixed, and
`test/smoke/clients/js/src/fixture.ts` carries a comment saying why it does
not. A unit test that counts worklet callbacks under an explicit downmix would
be cheaper than a full media run, if one can be made to fail reliably.

## Related

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - the other place browser-side capture and encode costs get measured
