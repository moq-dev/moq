# [M] Chromium's moq-transport audio keeps its tone through the replacement case

## Goal

`just test media --media-cases replacement --media-engines chromium` passes
its `audio tone: replacement moq-transport-22` check every run: the steady
audio after a replacement carries the fixture tone in at least 90% of
samples, as it does on the lite rows on the same host.

## Plan

Found by [moq-dev/moq#9293](https://github.com/moq-dev/moq/pull/9293), which
made the replacement and fallback join live and lifted the Firefox
quarantine. With that in, both engines' lite rows and Firefox's
`moq-transport-22` row passed every local run, and every fallback stayed
under 420 ms with no slow-group skips. Chromium's `moq-transport-22` row
still failed 2 of 3 local runs (host heavily loaded) on the tone check alone,
72/81 and 70/81 samples against a floor of 73. Main fails it the same way,
and the scheduled Interop failures the triage saw were the fallback storms
that PR fixed, so CI has not shown this one yet.

The audio bytes reaching the player stop for about 250 to 300 ms, then
arrive in a burst while video keeps playing. That points at delivery on the
relay-to-viewer moq-transport hop, not playback: each 20 ms audio group is
its own subgroup stream, and the ordered reader waits on a late one.

- First check whether the gap is stream credit: the relay's ietf publisher
  (`rs/moq-net/src/ietf/publisher.rs`) opening a stream per audio group
  against Chromium's MAX_STREAMS, versus how lite paces the same groups.
- Then whether the JS ietf subscriber (`js/net/src/ietf/subscriber.ts`)
  holds a group behind an earlier one that arrives late.
- Reproduce with
  `just test media --media-cases replacement --media-engines chromium`;
  `HARNESS_RUNS` needs a short path for the PulseAudio socket.

Public API: none expected. Wire: none expected.
