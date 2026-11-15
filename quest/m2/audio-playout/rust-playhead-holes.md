# [M] Native players stop waiting on a hole their clock passed

## Goal

`moq play` skips a missing group once its presentation clock reaches the
group past it, as the browser player does since
[moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799), instead of
holding the frames behind the hole for the whole hole wait.

## Plan

The browser's container consumer takes the player's clock as
`ConsumerProps.playhead` (`js/hang/src/container/consumer.ts`): a hole whose
successor starts at or before the media time presenting now ends then, and the
hole's timer also wakes when the clock gets there. Mirror it in
`rs/moq-mux/src/container/consumer.rs` (`poll_event_once`'s missing-group
branch and its `hole_timer`), reading `moq_mux::sync::Consumer::position`, and
pass the clock from `moq play`'s presentation through
`moq_audio::decode::Consumer` and the video reader in
`rs/moq-cli/src/play/media.rs`.

Exercise it with the same shape the browser hit: a reader that delivered a
pre-pause tail, then a replayed backlog arriving newest first behind a hole.
