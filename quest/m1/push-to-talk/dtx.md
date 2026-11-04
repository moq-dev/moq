# [M] Push-to-talk suppresses silence instead of toggling the catalog

## Goal

Muting a push-to-talk publisher keeps its audio rendition enabled and its
subscription live, and sends no media while silent. The next utterance reaches
a listener with no catalog flip and no round trip, and listeners play comfort
noise across the silence without counting it as an underrun. The hold design,
listeners staying subscribed to a disabled rendition, is retired in the same
change. `just test push-to-talk` proves it.

## Plan

Planned 2026-11-04 with the maintainer, from the
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760) latency
breakdown.

Decided 2026-11-04 (maintainer), reversing the epic's 2026-10-28 "Hold, not
DTX": a catalog toggle makes every listener wait out the `enabled` flip, so
the jitter buffer must hold at least a round trip for push-to-talk to work,
and the maintainer would rather fix DTX and avoid the catalog entirely.

- **Route: gate before the encoder, stamp from our own inputs ✅**, decided
  2026-11-04 (maintainer) from the measurements on Chrome, Firefox and Safari
  in [moq-dev/moq#7880](https://github.com/moq-dev/moq/pull/7880). Not chosen:
  WASM libopus 1.6.1 with DTX; the gate plus native `usedtx` on Firefox only.
  While muted, the publisher feeds the `AudioEncoder` nothing, and stamps
  each chunk from a queue of the input timestamps it fed, never
  `chunk.timestamp`: Chrome and Firefox stamp gated input as if it were
  contiguous (19 s ended at 10.6 s), and every browser emits exactly one chunk
  per input, in order, within 28 ms, so nothing waits in the codec across a
  gap. That also retires the encoder restart on an input gap in
  `js/publish/src/audio/encoder.ts`. `flush()` emits one extra chunk past the
  queue in every browser (the codec's lookahead tail); the queue must account
  for it.
- **Mute is the gate ✅**, decided 2026-11-04 (maintainer): the publisher's
  mute only. Not chosen: mute plus an energy VAD for open mic. No energy VAD
  until open-mic suppression has a consumer. One was measured in #7880
  (-45 dBFS, 200 ms hangover, one frame of pre-roll): no speech frame lost
  and the head within 5 ms of unsuppressed.
- Silence then sends nothing, where libopus DTX still sends comfort-noise
  updates (2.9 packets/s, 1.3 kbps); the listener makes its own comfort noise
  (below). Rejected: WASM libopus (the encoder is 117 KB gzip at `-Oz`; the
  151 KB `-O2` build took 1.5 to 2.2x the native encode time) and native
  `usedtx` (Chrome drifts 7.3 s over 19 s,
  Safari ignores it; see [Chromium's Opus encoder stamps chunks from their
  input](/quest/m2/chromium-opus-timestamps.md)).
- **Hold retires when this lands ✅**, not before, so push-to-talk keeps
  working on main meanwhile. Remove the players' default hold of a disabled
  rendition and restore the hang spec's "must not select" if nothing else
  relies on "should not select"; check video and exports before restoring.
- **Gaps play comfort noise ✅**, not plain silence. A suppressed span is
  intentional silence: it counts as no underrun, does not grow the buffer, and
  the jitter estimator ignores it
  ([Audio jitter target](/quest/m1/audio-jitter-target/README.md) specifies
  the estimator; record the exception there too).
- The publisher's mute becomes "suppress", not `enabled: false`. Keep
  `enabled` for a real stop (leaving the call), where a catalog change is
  correct.

Update the epic's acceptance and `doc/` pages that describe hold.
