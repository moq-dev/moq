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

The epic's own work: finish the acceptance proof with `just test push-to-talk`
(22 presses, 60 ms Opus, 300 ms delay) behind bufferbloat: a rate limit below the media
bitrate, behind a deep but finite queue whose overflow drops packets. That
gates acceptance. A held listener's unmute pays no catalog round trip and no
resubscribe. Every frame of the utterance is encoded and sent; any frames
dropped come only from the path. Rerun the reporter's setup (100 ms RTT, 2%
random loss) only as a reproduction of the original numbers, not as acceptance.
Record both in the PR.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760)):

- ✅ **Latency: at most 300 ms of playout on top of one-way delivery.** "We
  can't ever expect 300ms latency if it takes more than 300ms to deliver a
  packet." Queueing on the path (bufferbloat) is delivery, not playout, so the
  bufferbloat profile is not graded on latency, and no clocked live start or
  other startup machinery is added. The runner reports per separated press
  `delivery` (the slowest frame the onset waited on, from its first sample's
  capture, so packetization and retransmission count), `playout` (the rest, up
  to the render onset on the render clock), and projected device `output`
  apart. The player's target is its delay plus measured jitter plus one frame
  (`js/watch/src/audio/latency.ts`); the frame is packetization, already in
  delivery. On the reproduction at 2026-11-04 UTC, playout was 166/262/310 ms
  (min/median/max), with 14 of 15 presses at or under 300 ms and one at 310
  ms. Behind bufferbloat, for information, playout was 333/494/708 ms beside
  delivery of 310-546 ms, likely the jitter term following that spread (not
  attributed).
- ✅ **No encoder recovery on re-enable.** Asked first why a fatal encoder error
  is expected at all: it is not. No push-to-talk run reported one (the runner
  fails on `out.error`), and no issue reports a publisher encoder failure; the
  latch came from a review finding
  ([moq-dev/moq#5441](https://github.com/moq-dev/moq/pull/5441)). WebCodecs fails
  an encoder only for a config the platform rejects (`NotSupportedError`, which
  the same settings would hit again), an internal codec error (`EncodingError`),
  or reclamation (`QuotaExceededError`), which Chromium applies only to
  inactive, backgrounded hardware codecs; Opus is a software codec. A failure
  stays latched until a device, settings or `source` change.

The first full bufferbloat run also found one 60 ms frame submitted to
WebCodecs 0.3 ms before release that never produced output: disabling closes
the encoder synchronously and can discard queued codec work. It recurred in 3
of 7 runs on 2026-11-04, each a frame submitted under 1 ms before release.
[Buffered audio across stop](/quest/m1/audio-across-stop.md) keeps the codec
across a disable and flushes it before the discontinuity, which drains that
tail to its original track. Once it lands, rerun the proof; the runner
deliberately fails on this loss. A complete rerun whose reproduction holds the
latency goal above finishes this epic.

## Required

- [Buffered audio across stop](/quest/m1/audio-across-stop.md) - flushes the codec before a disable's discontinuity, so the last frame before a release is published

## Closes

- [#7101](https://github.com/moq-dev/moq/issues/7101) - a disabled audio rendition is deselected, so the start of the next utterance is lost
