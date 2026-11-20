# [S] Decode consumers take a subscription priority

## Goal

`moq_video::decode::Options` and `moq_audio::decode::Options` carry the
subscription priority their consumer fetches at, defaulting to hang's
`PRIORITY.video` and `PRIORITY.audio` as fixed today, so a player can fetch a
less important source behind a more important one under congestion.

## Plan

Each consumer hardcodes hang's priority for its kind today
(`rs/moq-video/src/decode/consumer.rs`, and its audio twin). Recommended: a
consumer that changes its priority while running updates its subscription in
place rather than resubscribing. Prove it with a test that reads the
priority off the publisher's aggregate subscription.

Needed by [the studio's viewer priorities](https://github.com/moq-dev/studio/blob/main/quest/m2/compose/controls.md),
found by [moq-dev/studio#218](https://github.com/moq-dev/studio/pull/218),
and by [moq play composites layers](/quest/m3/layers/play.md).
