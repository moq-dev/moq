# [L] libmoq: track demand and dynamic track requests

## Goal

A C publisher learns when a published track gains its first subscriber and
when it loses its last, and can serve track requests it did not declare up
front, so an encoder on a battery-powered device runs only while someone is
watching. The C ABI mirrors what moq-ffi already has: `used`/`unused` on
`MoqTrackProducer` and single-track `MoqMediaProducer`, and
`MoqBroadcastDynamic::requested_track` with `MoqTrackRequest::accept`/`abort`,
`publish_media_on_track` for accepting a request as media, and the group
request path (`MoqTrackDynamic::requested_group`, `MoqGroupRequest`) that
keeps a FETCH-driven request serviceable across acceptance.

Non-goals: no wire or public Rust API change; the OBS plugin does not adopt
the signal (OBS owns its encoders and cannot pause them from an output);
a multi-track container handle has no single demand and errors, as in moq-ffi;
broadcast-level request serving stays with the announce handle quests.

## Plan

Branch from dev, where libmoq already serves broadcast requests through
`moq_origin_dynamic` and `moq_broadcast_request_*`; follow that callback
convention exactly (a positive handle per event, then exactly one terminal
`<= 0` call, after which `user_data` is never touched).

Demand, one level-triggered watcher per handle:

- `moq_publish_track_demand(track, on_demand, user_data)`,
  `moq_publish_media_demand(media, on_demand, user_data)`, and the same on
  the built-in encoder handles, `moq_publish_video_raw_demand` and
  `moq_publish_audio_raw_demand` (moq-ffi's `MoqVideoProducer` and
  `MoqAudioProducer` carry `used`/`unused` too), return a watcher handle;
  `moq_publish_demand_close(watcher)` stops it.
- `on_demand(user_data, int32_t status)` fires immediately with the current
  state, again on every observed change, then once with a terminal `<= 0`. Positive
  values come from a `moq_demand` enum (`MOQ_DEMAND_USED`,
  `MOQ_DEMAND_UNUSED`). Seeding with the current state is what closes the
  race the issue calls out: a track that went unused before registration
  still reports it.
- Neither `Producer` nor `Demand` exposes the current state, only the
  level-triggered waits `used()` and `unused()`, and exactly one of them is
  ready at any instant. The watcher task races the two to learn the initial
  state, reports it, then loops waiting on the opposite wait. A flip between
  reporting and arming resolves the next wait immediately, so no edge is
  lost; a double flip collapses into no report, which is correct for a level
  signal. Do not add an `is_used` getter to moq-net for this.
- The media watcher refuses a container handle with the same error moq-ffi
  uses.

Requests, mirroring the broadcast path one level down:

- `moq_publish_dynamic(broadcast, on_request, user_data)` registers the
  handler and returns a handle; `moq_publish_dynamic_close` drops it. Without
  a live handler an unknown track name is refused, as today.
- `moq_track_request_name(request, dst)`, `moq_track_request_accept(request,
  info)` returning a `moq_publish_track` handle (the demand watcher attaches
  to it like any other), `moq_track_request_abort(request, error_code)`, and
  `moq_track_request_free(request)`. `info` is the existing `moq_track_info`,
  NULL for the microsecond default.
- `moq_track_request_video(request, init)` and
  `moq_track_request_audio(request, init)` mirror `publish_media_on_track`,
  split by kind the way `moq_publish_video`/`_audio` are: the importer
  accepts the request, sets the timescale, and returns the same media handle
  those return, so the demand watcher and frame writes work unchanged.
  Single-track formats only, as in moq-ffi.

Group requests, so a track requested by FETCH is served rather than resolving
`NotFound` once accepted:

- `moq_track_request_dynamic(request, on_group, user_data)` mirrors
  `MoqTrackRequest::dynamic`: obtain it before accepting when the track was
  requested by a fetch, so the pending group request survives the transition.
  `moq_publish_track_dynamic(track, on_group, user_data)` is the same handler
  on an already-published track (`MoqTrackProducer::dynamic`). Both return a
  handle closed by `moq_publish_dynamic_close`, and `on_group` follows the
  request callback convention.
- `moq_group_request_sequence`, `moq_group_request_priority`,
  `moq_group_request_accept(request)` returning a `moq_publish_group` handle,
  `moq_group_request_abort(request, error_code)`, and
  `moq_group_request_free`. Cached groups never reach the handler.

Landing: regenerate `moq.h` (build.rs does not regenerate it on src-only
changes), document every new symbol in `doc/lib/c`, and add tests in
`rs/libmoq/src/test.rs`: a consumer subscribing then closing drives
used -> unused; a watcher registered after the last consumer left is told
unused first; one dynamic request is accepted and the returned track
carries frames, a second is aborted and its subscriber sees the code.
Remove the dynamic-track bullet from #2152 when this lands.

## Closes

- [#3678](https://github.com/moq-dev/moq/issues/3678) - close this issue when the quest finishes

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the wider C ABI catch-up this splits out of
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - broadcast-level request serving on the C ABI
- [#3000](/quest/m1/3000-track-teardown-on-poll-unused-is-not-atomic-against-a.md) - an unused edge is not terminal; the watcher must keep reporting after it
