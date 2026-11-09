# [M] Native playback decodes audio only when the ring has room

## Goal

Native playback (`moq_audio::decode::Consumer` feeding `moq play`'s sink)
decodes an audio frame only when the ring has room below its floor. The rest
of the target delay stays encoded upstream, as in the browser, so native
memory follows the bytes actually received and the ring never grows with the
delay. Once this lands, Rust splits the target between decoded PCM and encoded
media the way JS does, and `moq play` holds no decoded PCM outside the ring.

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
Share names and tests with the JS side where the two can match. The gate's
input is the playhead the JS worklet reports. Native can derive it from
`Sink::buffered()` (`rs/moq-audio/src/playback/sink.rs`) and the last write's
timestamp, but `play_audio` owns that sink, not the consumer, so the consumer
needs it passed in. Keep the gate inside moq-audio's consumer if that stays
additive; if it needs a public API change, it moves to m2.

Public API: additive if the gate stays inside `moq_audio::decode::Consumer`.
Wire: none.

## Related

- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native time stretch this gate feeds
