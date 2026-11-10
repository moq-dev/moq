# [M] Native playback decodes audio only when the ring has room

## Goal

Native playback (`moq_audio::decode::Consumer` feeding `moq play`'s sink)
decodes an audio frame when the browser's `Backpressure` would: while playing,
once the playhead is within the ring's floor of it, and, live, once the media
queued behind it covers the part of the target held encoded, so a backlog past
the target still reaches the ring to be caught up. The rest of the target
delay stays encoded upstream, so native memory follows the bytes actually
received and the ring never grows with the delay. Once this lands, Rust splits
the target between decoded PCM and encoded media the way JS does, and
`moq play` holds no decoded PCM outside the ring.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8362](https://github.com/moq-dev/moq/pull/8362): "quest-plan
backpressure like JS does. So we don't decode audio until there's space in
the ring buffer"). ✅ m1, in this epic (rejected: m2, m3).

The browser already works this way: `Backpressure` in
`js/watch/src/audio/buffer.ts` decides when the decode loop may decode a frame,
holding the ring at its floor of PCM and the hold encoded in the container
consumer. Native decodes promptly instead: `Consumer::read` decodes each packet
as it is read, and the PCM waits in `play_audio`'s queue in
`rs/moq-cli/src/play/media.rs`. #8362 keeps a delay past the ring's budget in
sync by parking that PCM there until it is due. This quest replaces that queue
with the gate, so the hold stays encoded.

Mirror the browser's rules, including the stalled refill that starts playback
with the whole target ahead of the playhead, rather than inventing new ones.
Share names and tests with the JS side where the two can match.

The gate waits before decoding, never before reading. `Consumer::read` stamps
each packet's arrival for the jitter estimate (`target.observe` in
`rs/moq-audio/src/decode/consumer.rs`), and `Consumer::delay()` documents that
reads paced to the speaker make the estimate measure the pacing. Keep reading
and observing encoded packets as they arrive and hold them encoded, as
`js/watch` reads a frame and only then awaits `ring.wait()`.

The gate has two inputs, and `play_audio` owns both, so the consumer needs
them passed in:

- The playhead the JS worklet reports. Native can derive it from
  `Sink::buffered()` (`rs/moq-audio/src/playback/sink.rs`) and the last
  write's timestamp.
- The shared playout delay: the catalog `delay` plus the target, the largest
  among the subscribed renditions, which the browser passes as
  `sync.out.delay` and `play_audio` already holds the sink to through
  `presentation.due`. `Consumer::delay()` is this track's jitter target alone;
  a gate on it would decode at a depth the sink has already outrun and could
  not replace #8362's hold.

Decided 2026-11-09 (maintainer, /quest-unblock): the gate must not start
audio early when a shared catalog delay is under the ring's cap, with a test.
✅ Fix it here (rejected: pad the missing lead in
[moq-dev/moq#8362](https://github.com/moq-dev/moq/pull/8362)). With a shared delay
that fits the sink's cap, that PR writes the frame without its hold-path lead
padding, so audio can start up to ~150 ms early
(`rs/moq-cli/src/play/media.rs`). The browser-mirroring refill starts with the
whole target ahead of the playhead and replaces that padding path, so the fix
lands with the gate.

Keep the gate inside moq-audio's consumer if that stays additive; if it needs
a public API change, it moves to m2.

Public API: additive if the gate stays inside `moq_audio::decode::Consumer`.
Wire: none.

## Related

- [Stretch model](/quest/m1/audio-jitter-target/stretch.md) - requires this gate, then changes the stalled refill mirrored here to a small cushion in both players
