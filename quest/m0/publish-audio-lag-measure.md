# [S] js/publish: measure the audio encoder's input-to-output lag on a high-RTT path

## Goal

A `<moq-publish>` session on a real microphone against a relay with 40 ms or
more of added RTT emits every encoded audio chunk within one frame duration of
its input, measured over 25 seconds or more, with and without subscriber churn.

## Plan

One root cause is already fixed: the `AudioEncoder` was rebuilt whenever the
rendition's track producer changed, which discarded whatever the codec still
held and restarted the framer mid-frame, and it was built only once demand
arrived, putting `configure()` on the critical path of the first subscription.
That is covered by unit regressions in `js/publish/src/audio/encoder.test.ts`,
but it was never measured against a real relay, so the numbers #3477 reported
stand unconfirmed: a lag that grew to 7.35 s over 25 s on the public relay, and
a churn-free drift of 88 to 275 ms/s against 0.7 ms/s on localhost.

- Measure with the reporter's instrumented harness (fork branch
  `debug/rt-audio`: timestamps at capture, encoder input and output, and
  `writeFrame`). Chrome's fake microphone is unusable, its encoder lags by
  design.
- If drift survives, the suspects are what the fix did not touch: `writeFrame`
  opening a group per audio frame under WebTransport stream credit, and the
  main-thread task queue that delivers encoder output.
- Complete this when the measurement is clean, or turn what it finds into a
  root-cause quest.

## Related

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - the report the numbers come from
