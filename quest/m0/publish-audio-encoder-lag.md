# [M] js/publish: AudioEncoder output falls behind its input after a subscriber churn

## Goal

`<moq-publish>` emits every encoded audio chunk within one frame duration of
its input, for the life of a session, regardless of how many watchers join
or leave. Today the encoder's output intermittently lags its on-time input by
seconds on a 45 ms RTT path, which every watcher then hears as stutter no
receive buffer can fix.

## Plan

Observed in #3477, not root-caused, so reproduce first. Three times, right
after a watcher left and another subscribed (the relay aborts the publisher's
tracks, then `publish ok` again), `AudioEncoder` output started arriving late
while capture, `encodeQueueSize` (0), the `VideoEncoder`, and
`track.writeFrame` (p99 0.8 ms) all stayed on time: locally a transient 100
to 170 ms, on the public relay a lag that grew to 7.35 s over 25 s and stayed
(4511 chunks emitted for 4878 inputs). In two later runs the lag began with
the first subscription and no churn, drifting 88 to 275 ms/s. The same
publisher on localhost drifted 0.7 ms/s. Chrome's fake microphone is not
usable for this, its encoder lags by design; measure with a real device.

- Reproduce with the reporter's instrumented harness (fork branch
  `debug/rt-audio`, timestamps at capture, encoder input and output, and
  `writeFrame`) against a relay with 40 ms or more of added RTT, with and
  without subscriber churn.
- Chunks emitted fall short of inputs, so frames are being dropped or
  coalesced somewhere between `AudioEncoder.output` and the track write: check
  the encoder's flush and reconfigure paths on track abort, whether the
  publisher rebuilds the encoder on `publish ok`, and whether backpressure
  from the track (the write awaiting a group the relay has not accepted)
  stalls the callback that dequeues encoder output.
- Fix at the source and add a regression that counts chunks against inputs
  across a simulated subscriber churn.

## Related

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - the report that observed this
