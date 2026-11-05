# [M] Push-to-talk loses no speech

## Goal

A listener subscribed while a publisher is muted hears the next utterance from
its first frame. Unmuting costs no catalog round trip, no resubscribe and no
microphone reopen. A muted rendition still costs no media bytes.

## Plan

From [#7101](https://github.com/moq-dev/moq/issues/7101), traced 2026-10-28. With
`Audio.Encoder` `enabled: false`, watch deselects the rendition and
unsubscribes. On unmute the listener waits for the catalog flip, then its
SUBSCRIBE reaches the publisher. The demand gate discards everything encoded
before that (`js/publish/src/audio/encoder.ts:686`). At 100 ms RTT that is
three one-way trips plus the probe and a frame (~515 ms to the first fresh
frame). The emptied buffer then refills the 300 ms delay, so the result is
~820-950 ms to audible, with 250-400 ms of speech never encoded.

Decided 2026-10-28 (maintainer):

- **Hold, not DTX.** DTX is for silence the app can't predict. Push-to-talk
  knows when it is silent, and a held subscription costs no media bytes and no
  encoder CPU. DTX is off on main anyway ([#4919](https://github.com/moq-dev/moq/pull/4919)).
  **Reversed 2026-11-04 (maintainer):** a catalog toggle makes every listener
  wait out the `enabled` flip, so the jitter buffer must hold at least a round
  trip. Push-to-talk moves to DTX ([Push-to-talk suppresses
  silence](/quest/m1/push-to-talk/dtx.md)), and hold retires when that lands;
  until then hold stays on main.
- **Players hold a disabled rendition**, audio and video, by default, in
  every first-party player. Exports keep dropping it, since a held track
  sends nothing and an interleaving muxer or HLS player may wait on it. The
  hang spec weakens from "must not select" to "should not select" a disabled
  rendition.
- **The publisher keeps its probe result across a disable**, and the
  element's `muted` and `invisible` leave capture open.
- **Encoding no longer waits for demand**, in every first-party publisher
  (finished by [moq-dev/moq#7554](https://github.com/moq-dev/moq/pull/7554)).
  A listener who
  subscribes after the unmute still gets the start within its `delay.max`. This
  replaces a publisher head buffer, which was considered and dropped as the
  same idea special-cased.

The epic's own work: the acceptance proof `just test push-to-talk` (22
presses, 60 ms Opus, 300 ms delay). A held listener's unmute pays no catalog
round trip and no resubscribe, and every frame of each utterance is encoded,
sent, received, and decoded from its first frame.

Decided with the maintainer on
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760):

- ✅ **Ideal conditions only** (2026-11-04): "don't use random packet loss
  either, assume ideal conditions." Both elements reach a local relay
  directly, on separate sessions so the media crosses it, with no shaping,
  loss, or queueing. The bufferbloat and 2% loss profiles were removed.
- ✅ **Latency: at most 300 ms of playout on top of one-way delivery**
  (2026-11-03): "We can't ever expect 300ms latency if it takes more than
  300ms to deliver a packet." No clocked live start or other startup
  machinery. Five ideal runs on 2026-11-04 held it on all 75 separated presses
  (median 261 ms, max 294 ms). The breakdown and its knobs are in
  [Push-to-talk latency](/quest/m1/push-to-talk-latency.md).
- ✅ **No encoder recovery on re-enable** (2026-11-03). Asked first why a
  fatal encoder error is expected at all: it is not. No push-to-talk run
  reported one, and no issue reports a publisher encoder failure; the latch
  came from a review finding
  ([moq-dev/moq#5441](https://github.com/moq-dev/moq/pull/5441)). WebCodecs
  fails an encoder only for a config the platform rejects, an internal codec
  error, or reclamation, which Chromium applies only to inactive, backgrounded
  hardware codecs; Opus is software. A failure stays latched until a device,
  settings or `source` change.

The publisher's tail drop is fixed:
[moq-dev/moq#7725](https://github.com/moq-dev/moq/pull/7725) flushes the codec
before a disable's discontinuity. A frame submitted under 1 ms before release
used to be lost in 3 of 7 runs; it was lost in none of 12 since.

What remains is a lost head at the listener. In one of 110 presses over the
2026-11-04 relay runs (run 3, press 20, after a 60 s mute), the
head's group 918 and the next frame's group 919, written at +72 and +73 ms,
reached the listener reordered: 919 at +187 ms and 918 at +192 ms, about 115
ms in transit against the usual 3 ms. The listener decoded 919 at +191.7 ms.
Group 918 arrived 0.4 ms later and never reached the decoder, so the utterance
played from its second frame, 60 ms late. The JS ordered read still delivers a
group below its cursor as a late backfill
(`js/hang/src/container/consumer.ts`), so the head was most likely dropped as
late between the read and the decoder (`js/watch/src/audio/decoder.ts`); pin
that down first. After a declared end, the next group is known to be the end's
group plus one. Holding for it within the delay budget would keep the head
without adding startup latency, but weigh that against the no-window decision
in [Container reads drop backfill](/quest/m1/read-backfill.md).

The runner fails on a head that is received but never decoded, so the proof
catches this. A complete set of runs finishes this epic.

## Required

- [Push-to-talk suppresses silence instead of toggling the catalog](/quest/m1/push-to-talk/dtx.md) - replaces hold with DTX

## Closes

- [#7101](https://github.com/moq-dev/moq/issues/7101) - a disabled audio rendition is deselected, so the start of the next utterance is lost

## Related

- [Container reads drop backfill](/quest/m1/read-backfill.md) - changes how the ordered read treats a group below its cursor, where this head was lost
