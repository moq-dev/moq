# [S] moqsrc resumes after a redial without replaying what it rendered

## Goal

After `moqsrc`'s session redials and the same broadcast instance returns, its
pads continue at the live edge: no group already rendered plays again, and
playback runs no later than before the redial.

## Plan

Found by [moq-dev/moq#9168](https://github.com/moq-dev/moq/pull/9168), whose
test clock made it visible in
`source::imp::session_tests::a_protocol_violation_redials_after_playback`
(`rs/moq-gst/src/source/imp.rs`): after the relay closes the session, the new
session replays every cached group of the same instance from pts 0 under a
fresh timeline based at the redial, so all later frames play 500ms behind,
up to `max-delay` (1s by default), and each redial can add that again.
A wall clock hid it because the synced sink just waited.

Recommendation: keep the instance's `Timeline` across the redial (the
follower already keeps one per instance in `resumable`) and resume each
rendition after the last group it pushed, so replayed groups are skipped as
stale. Then make `rendered` in that test fail on a frame the sink holds early,
which today it lets the clock catch up to.
