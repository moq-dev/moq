# [M] libmoq: track demand and dynamic track requests

## Goal

A C publisher learns when a published track gains its first subscriber and
when it loses its last, and can serve track requests it did not declare up
front, so an encoder on a battery-powered device runs only while someone is
watching. The C ABI mirrors what moq-ffi already has: `used`/`unused` on
`MoqTrackProducer` and single-track `MoqMediaProducer`, and
`MoqBroadcastDynamic::requested_track` with `MoqTrackRequest::accept`/`abort`.

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

- `moq_publish_track_demand(track, on_demand, user_data)` and
  `moq_publish_media_demand(media, on_demand, user_data)` return a watcher
  handle; `moq_publish_demand_close(watcher)` stops it.
- `on_demand(user_data, int32_t status)` fires immediately with the current
  state, again on every edge, then once with a terminal `<= 0`. Positive
  values come from a `moq_demand` enum (`MOQ_DEMAND_USED`,
  `MOQ_DEMAND_UNUSED`). Seeding with the current state is what closes the
  race the issue calls out: a track that went unused before registration
  still reports it.
- Implement by alternating `used()`/`unused()` on the track (or the media
  producer's `Demand`) inside the watcher task; the media watcher refuses a
  container handle with the same error moq-ffi uses.

Requests, mirroring the broadcast path one level down:

- `moq_publish_dynamic(broadcast, on_request, user_data)` registers the
  handler and returns a handle; `moq_publish_dynamic_close` drops it. Without
  a live handler an unknown track name is refused, as today.
- `moq_track_request_name(request, dst)`, `moq_track_request_accept(request,
  info)` returning a `moq_publish_track` handle (the demand watcher attaches
  to it like any other), `moq_track_request_abort(request, error_code)`, and
  `moq_track_request_free(request)`. `info` is the existing `moq_track_info`,
  NULL for the microsecond default; codec configuration stays in the catalog
  and is not a request parameter.

Landing: regenerate `moq.h` (build.rs does not regenerate it on src-only
changes), document every new symbol in `doc/lib/c`, and add tests in
`rs/libmoq/src/test.rs`: a consumer subscribing then closing drives
used -> unused; a watcher registered after the last consumer left is told
unused first; a dynamic request is accepted and then aborted. Remove the
dynamic-track bullet from #2152 when this lands.

## Closes

- [#3678](https://github.com/moq-dev/moq/issues/3678) - close this issue when the quest finishes

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the wider C ABI catch-up this splits out of
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - broadcast-level request serving on the C ABI
- [#3000](/quest/m1/3000-track-teardown-on-poll-unused-is-not-atomic-against-a.md) - an unused edge is not terminal; the watcher must keep reporting after it
