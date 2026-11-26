# [S] Transcoder stamps derivative catalog updates on the source timeline

## Goal

`moq-transcode` publishes each derivative catalog update at the timestamp the source's catalog update carried, on the source's timeline, so a reader that holds updates until their time (a player behind the live edge) applies a rendition change when the media it concerns plays, for a clockless source as well as a clocked one.

## Plan

Found by the review of [moq-dev/moq#9714](https://github.com/moq-dev/moq/pull/9714). `Transcoder::run` stamps every derivative catalog update with `derived.next_timestamp()`, a reading of the derivative's own clock. Rungs republish the source's media timestamps, so for a source with no `sync.clock` the derivative's clock (starting ten seconds in) shares nothing with them: a change at source PTS 1s lands at roughly 10s and is late or never applied by a delayed reader. A clocked source hides it only because the derivative adopts its mapping.

- `moq_mux::catalog::hang::Consumer::poll_next` drops the update's timestamp; surface it additively (a method returning the timed snapshot).
- Pass it to `derived.modify(at)`. `Producer::modify` re-bases `at` when it re-bases the clock (`restamp` in `rs/moq-mux/src/catalog/producer.rs`), which is right for a stamp read on the producer's own clock but shifts a source timestamp: stamp the first update after the clock is fixed, or make that path leave a caller's source stamp alone.
- Stamps must stay past the last published one (`TimestampRewind`): decide what a source catalog that republishes at an earlier timestamp does (clamp to the last, with a warning).
- Test with a clockless source whose rendition change is at a media PTS far from the derivative's clock.

Public API: additive only.
