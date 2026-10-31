# Push-to-talk loses no speech

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
- **Encoding no longer waits for demand**
  ([always-on encoding](/quest/m1/eager-encode/README.md)). A listener who
  subscribes after the unmute still gets the start within its `maxDelay`. This
  replaces a publisher head buffer, which was considered and dropped as the
  same idea special-cased.

The epic's own work: once the children land, measure push-to-talk (22
presses, 300 ms delay) behind bufferbloat: a rate limit below the media
bitrate, behind a deep but finite queue whose overflow drops packets. That
gates acceptance. A held listener's unmute pays no catalog round trip and no
resubscribe, so unmute → audible is the path's one-way delay plus the 300 ms.
Every frame of the utterance is encoded and sent; any frames dropped come
only from queue overflow. Rerun the reporter's setup (100 ms RTT, 2% random
loss) only as a reproduction of the original numbers, not as acceptance.
Record both in the PR.

Also check failure recovery (found 2026-10-28 by
[moq-dev/moq#7157](https://github.com/moq-dev/moq/pull/7157)): unmuting used to
reopen the microphone, which made a new source and so recovered a failed
encoder. Capture now stays open, so only a device swap, a settings change or a
new `source` recovers one. Decide whether re-enabling should retry a failed
encoder, and record why.

## Required

- [Watch caches audio codec support](/quest/m1/push-to-talk/audio-support-cache.md) - an unmute resumes audio without waiting on a support check

## Closes

- [#7101](https://github.com/moq-dev/moq/issues/7101) - a disabled audio rendition is deselected, so the start of the next utterance is lost

## Related

- [Always-on encoding](/quest/m1/eager-encode/README.md) - encoders run while enabled, so a late subscriber can fetch the start
