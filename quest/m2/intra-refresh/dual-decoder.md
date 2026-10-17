# [M] Browser viewers stay live through refresh recovery with two decoders

## Goal

A js/watch viewer of a video rendition with `warmup` set never freezes or
waits for a refresh sweep. A min-delay decoder holds the app's latency and
may show refresh stripes right after a cold join or a skip. A max-delay
decoder tolerates one more sweep of delay, so it keeps continuity and its
picture is clean. Whenever both have a frame for the current playback time,
the max-delay picture is shown. Stripes appear only while the max-delay
decoder is catching up and both decoders are unrecovered. A rendition without `warmup` uses
one decoder exactly as today.

## Plan

Decided in the 2026-10-14 planning interview:

- Replaces the freeze rule for players. Live with stripes is better than a
  frozen picture. Rust players do the same in `moq_video::decode::Consumer`
  (`rs/moq-video/src/decode/select.rs` holds its switch rule); only
  the transcoder avoids stripes, by decoding history per
  [consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md).
- Budgets: the min-delay decoder skips at the app's latency target (today's
  `sync.out.maxDelay`). The max-delay decoder skips at that target plus the
  catalog `warmup`, which is one refresh sweep. Two decoders run, both for the
  whole session, whenever the budgets differ, so `warmup` gates the feature.
  A missing or zero `warmup` keeps one decoder.
- One playback clock. Only the min-delay decoder drives `Sync` (`received`,
  `reset` on a break). The max-delay decoder decodes past-due frames as fast
  as it can without moving the clock, so it doesn't raise latency.
- No withhold rule and no catalog precision dependency. The max-delay decoder
  joins one group earlier, so the frames before its sweep completes are all
  behind the playhead. They never win the timestamp comparison. A
  rounded-down or stale `warmup` only shrinks its budget and costs extra
  stripes, never a dirty frame the min-delay decoder wasn't also showing.
  This quest therefore does not wait on
  [plan-boundary](/quest/m2/intra-refresh/plan-boundary.md).
- Selection is a small frame selector over the two current candidates: show
  the max-delay frame when its timestamp is at or beyond the min-delay frame's,
  so it wins ties, else the min-delay frame. Separately, never display a
  timestamp below the last one shown. A max-delay decoder that stops producing
  therefore loses as soon as the min-delay one advances. Do not reuse `caughtUp` from
  `js/watch/src/video/playhead.ts`: its 100 ms slack tolerates one backwards
  step per rendition switch, and applied per frame it would alternate
  backwards and forwards. Keep the pair per rendition, inside or beside
  `DecoderTrack` in `js/watch/src/video/decoder.ts`, so rendition switching
  still promotes a whole pair through the existing pending/active path.
- Each decoder opens its own subscription through `subscribeMedia`
  (`js/watch/src/media.ts`); a `Container.Consumer` consumes its subscriber.
  `js/net` dedups the two into one upstream request with the larger
  `maxDelay` (`combineSubscriptions`, `js/net/src/track.ts`), so live groups
  arrive once. The second decoder's only extra transfer is the earlier group it
  joins on, up to one refresh group per cold join or gap.
- The max-delay decoder reads groups oldest first. Subscriptions default to
  descending order and aggregate that way, so a cold join can activate the
  newest group in `Container.Consumer` first, and `DecoderTrack` then drops
  the earlier group as stale. That would leave the max-delay decoder starting
  mid-sweep. Its consumer must wait for the lower group within its budget
  before emitting a newer one, through the opt-in in
  [join earlier group](/quest/m2/intra-refresh/join-earlier-group.md), which
  fixes it at the source for Rust and `js/hang` alike.
- H.264 recovery-point frames sent as `key` chunks pass Chromium's WebCodecs
  keyframe check (`ParseH264KeyFrame` accepts `is_sei_recovery_point`; WPT
  `videoDecoder-h264-sei`). Chromium rejects a non-IRAP H.265 frame sent as
  `key`, so an HEVC refresh rendition fails through the decoder's existing
  `error` signal; no special case. Firefox and Safari behavior is unknown:
  check H.264 on both during verification.
- Tests in `js/watch/src/video/decoder.test.ts` with a synthetic `warmup`
  catalog and track: a cold join shows the min-delay picture and then switches
  to max-delay once it is caught up; a stall shorter than one sweep makes the
  min-delay decoder skip while max-delay stays continuous and takes over again;
  a stall longer than one sweep skips both; a rendition without `warmup` opens
  one subscription and one decoder; only the min-delay decoder moves `Sync`;
  a selector regression that repeatedly crosses a 100 ms trailing gap and a
  recovery boundary never displays a lower timestamp; a max-delay decoder that
  stops producing while the min-delay one continues hands the picture back on
  the next min-delay frame.
- Measure the cost of always running two decoders: CPU, decoder memory, and
  dropped frames for one versus two decoders on a 1080p rendition, in a
  browser, recorded in the PR.

## Required

- [Join earlier group](/quest/m2/intra-refresh/join-earlier-group.md) - the `js/hang` opt-in the max-delay decoder joins with

## Related

- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - publishes the real `warmup` for an end-to-end x264 check
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - the transcoder decodes history so it never encodes stripes
