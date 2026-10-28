# Migrating moq-c

How to update your code for each breaking change, newest first.

## Unreleased

### Authored configs keep their identity ([#7110](https://github.com/moq-dev/moq/pull/7110))

`moq_publish_video_config` and `moq_publish_audio_config` return `MOQ_ERROR_MUX` for a repeat call that changes the rendition's `codec`, `description`, or `container`, or an audio `sample_rate` or `channel_count`.
Publish media with a new identity under a new name, and remove the old rendition.
Tracks published with `moq_publish_video` or `moq_publish_audio` take a new name such as `<name>/1` when the stream's decoder identity changes, so read the name from the catalog rather than caching it.
A frame passed to `moq_encode_video_frame` that moves the rendition to a new track, such as a resume at a ceiling past the declared level, reserves bandwidth against the new track, so call `moq_encode_video_reservation` again rather than holding the first handle.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Every function taking a path, such as `moq_origin_create_broadcast` and `moq_origin_request`, returns an error for a path with a leading, trailing, or doubled `/` instead of collapsing it. Pass the canonical path, such as `room/cam`.

### Request handlers are named requests ([#6752](https://github.com/moq-dev/moq/pull/6752))

The request-handler functions are renamed to match moq-net's `requests()`; their arguments and callbacks are unchanged:
`moq_origin_dynamic` and `moq_origin_dynamic_cancel` are `moq_origin_requests` and `moq_origin_requests_cancel`,
`moq_publish_dynamic`, `moq_publish_track_dynamic`, and `moq_track_request_dynamic` are `moq_publish_requests`, `moq_publish_track_requests`, and `moq_track_request_requests`,
and `moq_publish_dynamic_cancel` is `moq_publish_requests_cancel`.

### Session allocator ([#6751](https://github.com/moq-dev/moq/pull/6751))

`moq_session_bandwidth` is `moq_session_allocator`, `moq_bandwidth_reserve` is `moq_allocator_reserve`, and `moq_bandwidth_close` is `moq_allocator_close`, matching `moq_net::bandwidth::Allocator`. `moq_reservation_grant` is `moq_reservation_rate`, matching `moq_net::bandwidth::Reservation::rate()`. Signatures are unchanged. The receive estimate keeps its names (`moq_session_recv_bandwidth`, `moq_bandwidth_consumer_*`).

### `moq_encode_video` takes a `moq_video_encode_options`

`moq_encode_video`'s fourth argument is a `const moq_video_encode_options *` instead of a `uint32_t bandwidth`: pass `NULL` where you passed `0`, or `&(moq_video_encode_options){.bandwidth = bandwidth}`.
Its `lower` field takes the handle of the next lower rendition's encoder, to disable this one while its bandwidth grant cannot fund it; that needs a `bandwidth` and a bitrate above the lower encoder's.
`moq_reservation_rate` reports the slice the track would get back while nothing subscribes to it, instead of no grant.

### Decoders deliver reads

`moq_decode_audio` and `moq_decode_video` call back with a read id per decoded frame, gap, or encoder end instead of a frame id. Replace `moq_decode_audio_frame` / `moq_decode_video_frame` with `moq_decode_audio_read` / `moq_decode_video_read`, which fill a `moq_audio_read` / `moq_video_read`: check `kind` (`MOQ_DECODE_READ_FRAME`, `MOQ_DECODE_READ_GAP`, `MOQ_DECODE_READ_END`) and take the frame from its `frame` field, the lost span from `gap`, or `end_us`. Free every id with `moq_decode_audio_read_free` / `moq_decode_video_read_free` where you called the `_frame_free` functions. Skip gap and end reads to keep the old behavior, or restart playback there, since what follows does not continue what came before.

### Sessions report a lifecycle snapshot

`moq_session_connect` and `moq_session_request_accept` take a `moq_state_callback` instead of a `moq_status_callback`: `void on_state(void *user_data, const moq_connection_state *state)`.
Each change delivers a `moq_connection_state` with `phase`, `connects`, `failure`, and the live `protocol`; copy what you keep, since it is valid only during the callback.
Read `state->connects` where you read a positive status code. A failed dial or dropped connection now fires the callback with a negative `failure`, so drop your `moq_session_failure` polling; `moq_error()` describes it during the callback.
The terminal call has phase `MOQ_CONNECTION_PHASE_CLOSED`, with `failure` zero after a requested close or the error that ended the session, where the terminal status code used to be. Release `user_data` there, as before.

### Exact encoder rates and publication options

`moq_video_encoder_input.framerate` is now `moq_video_frame_rate { numerator,
denominator }`, changing the C ABI layout. Replace `30` with `{30, 1}` or use
`{30000, 1001}` for an exact fractional rate. Both components must be nonzero;
rates above 1,000,000 fps are refused. Recompile callers with the generated header
and matching native library. Invalid rational rates return `MOQ_ERROR_INVALID_CONFIG`.
Audio and publication track arguments are unchanged.

### Video rendition preference

`moq_video_config` gains `int32_t preference`. Initialize it to `0` to keep the default ranking; higher values are preferred and compatibility transcodes use `-1`. Rebuild C consumers against the matching header and library because the struct layout changes.

### A broadcast reference waits for its sibling's announcement (#6254)

`moq_consume_video` and `moq_consume_audio` on a rendition whose catalog `broadcast` field names a sibling now wait for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, give up on it yourself with `moq_consume_video_cancel` or `moq_consume_audio_cancel`.

### JSON snapshot `delta_ratio` is `deltas`

`moq_json_snapshot_config.delta_ratio` is replaced by `bool deltas`.
Set `deltas = false` where you set `delta_ratio = 0`, and `deltas = true` for any positive ratio.
With deltas on, the track carries a refresh ratio of 8 and the producer restates the value in a new group when a joiner asks.

### Priorities are signed (#6142)

`moq_track_info.priority` and `moq_subscription.priority` are `int8_t`, and `moq_group_request_priority` writes an `int8_t`, which changes the ABI.
Higher is first and 0 is the unset midpoint, so a zeroed `moq_track_info` now uses the default priority rather than the least urgent one.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `moq_origin_announced` or `moq_origin_announced_broadcast`, instead of under its whole scope at connect.
`moq_origin_request` watches nothing, so on a client it fails with a negative code for a path nothing watches, even when the relay announces it.
Resolve the path with `moq_origin_announced_broadcast`, or hold `moq_origin_announced` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `MOQ_CONTAINER_FORMAT_FMP4` unwraps to Legacy (#6250)

A container import with `MOQ_CONTAINER_FORMAT_FMP4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `MOQ_CONTAINER_FORMAT_CMAF` to keep publishing CMAF.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `MOQ_ERROR_TIMESTAMP_OVERFLOW` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

`moq_publish_track_finish`, `moq_publish_media_finish`, `moq_publish_container_finish`, and the encoder finishes now release the handle while the broadcast keeps the finished track listed, with its catalog rendition and demand, until `moq_publish_close`.
Publish a replacement under a new name, and expect a finished track's demand terminal callback when the broadcast closes rather than at the finish.

### A claim answers by publishing (#5678)

`moq_origin_dynamic` takes no route, and `moq_origin_dynamic_update` is gone.
`moq_broadcast_request_accept` is replaced by `moq_broadcast_request_prepare`, which returns a prepared answer under the epoch you pass: fill its broadcast, then `moq_broadcast_prepared_publish` announces it at the exact path under that epoch.
`moq_broadcast_prepared_free` refuses an answer you will not publish.

### `moq_track_info` gains `compress` (#5928)

The new trailing field changes its ABI. A zero-initialized struct leaves it false, as before.

### Anchors can name a time (#6074)

`moq_anchor` gains `timestamp_us`, read with the new `MOQ_ANCHOR_TIME` kind, which changes its ABI.
`moq_consume_track_update` fails with a negative code when the track refuses an update, such as a time anchor on an untimed track, leaving the subscription as it was.
An update made before the subscription is established applies once it is, so a refusal then ends the consumer through its callback.

### JSON values carry their timestamp (#5677)

`moq_json_value` gains `timestamp_us` and `timestamp_present`, which changes its ABI.
A snapshot consumer's callback now fires for each state in order instead of only the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A raw track consumer now reads only the groups its `ranges` cover, rather than every group from the lowest start to the last absolute end.

### Demand reports the wanted group (#5930)

`moq_track_info` gains `refresh` and `refresh_present`, which changes its ABI; a zeroed field leaves refresh off.
A demand watcher's `on_demand` now repeats `MOQ_DEMAND_USED` whenever the wanted group moves, so treat it as "read the wanted group" rather than an edge.
Read it with `moq_publish_demand_wanted`, and start a new group (a keyframe) once it passes the largest group written.
Before any group is written, the first subscriber wants group 0, so start one whenever it is present.

```c
// `written` is false until a group is written; `largest` is the largest group written.
uint64_t group;
bool present;
if (moq_publish_demand_wanted(watcher, &group, &present) == 0 && present && (!written || group > largest)) start_group();
```

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `moq_client_config::quic_keylog` or `moq_server_config::quic_keylog` as well to capture them, each a field appended to its struct; without `SSLKEYLOGFILE` the dial or listen fails.

### OpenH264 is a build feature (#5771)

The release archives and the CMake build still carry the software H.264 fallback.
A bare `cargo build -p moq-c` leaves it out; add `--features openh264` to keep it.

### Subscriptions carry group ranges (#5732)

`moq_subscription`'s `group_start`, `group_end`, and their `_present` flags are replaced by `ranges` and `ranges_len`, an array of `moq_range` whose ends are `moq_anchor`s, and `order` (`moq_order`), which changes its ABI.

### Receive estimates come from `moq_session_recv_bandwidth` (#5777)

`moq_connection_stats` drops `estimated_recv_rate_bps` and `estimated_recv_rate_valid`, which changes its ABI.
Hold the consumer `moq_session_recv_bandwidth` returns to request PROBE, read it with `moq_bandwidth_consumer_peek` or wait with `moq_bandwidth_consumer_changed`, and release it with `moq_bandwidth_consumer_close`.

```c
int32_t consumer = moq_session_recv_bandwidth(session); // 0 when the version has no PROBE
if (consumer > 0) {
	uint64_t bps;
	bool present;
	if (moq_bandwidth_consumer_peek(consumer, &bps, &present) == 0 && present) use(bps);
	moq_bandwidth_consumer_close(consumer);
}
```

### `moq_origin_announced_broadcast` delivers every publisher instance (#5700)

Its callback gets a new broadcast handle each time another instance takes over the path or the broadcast returns, and its terminal `0` only on cancel.
Free each handle with `moq_consume_close` once its successor arrives.

### A timed track refuses a group that rewinds (#5803)

A frame write fails with a protocol error of kind `MOQ_PROTOCOL_KIND_TIMESTAMP_MISMATCH` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### libmoq is moq-c (#4288)

The C bindings ship as the `moq-c` crate, starting at 0.7.0.
The header is still `moq.h` and the library is still `libmoq.a` / `moq.lib` (`-lmoq`); the C ABI did not change with the rename.
The CMake package, pkg-config name, release tags, and archives follow the new name.

| Before | After |
| --- | --- |
| `find_package(moq)`, target `moq` | `find_package(moq-c)`, target `moq::c` |
| `pkg-config --libs moq` | `pkg-config --libs moq-c` |
| nix `.#libmoq` | nix `.#moq-c` |
| tags `libmoq-v*`, archives `libmoq-<version>-<target>` | tags `moq-c-v*`, archives `moq-c-<version>-<target>` |

### Background panics are reported (#5562)

A panic in a background task reports `MOQ_ERROR_PANIC`, and other handles keep running.

### Announcements carry a kind (#4266, #4384, #5087)

`moq_announce_update` is `moq_announce_event`, and its `active` flag is a `kind`: `MOQ_ANNOUNCE_KIND_START`, `_UPDATE`, `_END`, or `_RESTART`.
`_RESTART` means another publisher instance now serves the prefix: request the path again.
Subscribe on `MOQ_ANNOUNCE_KIND_START` or `_RESTART` where you checked `active`.

### `moq_origin_announced` takes a config (#4111)

The prefix and filter arguments moved into a `moq_announce_config`; pass `NULL` to receive everything.

```c
moq_announce_config config = { .prefix = prefix, .prefix_len = prefix_len };
moq_origin_announced(origin, &config, on_announce, user_data);
```

### Frames say whether they are timed

`moq_frame` and `moq_datagram` gain `timestamp_present`, which changes their size, so recompile against the new `moq.h`.
A frame read from an untimed track has `timestamp_present` false.

```c
if (frame.timestamp_present) use(frame.timestamp_us);
```

### Subscriber staleness is `max_delay_us`

`moq_subscription`, `moq_audio_decoder_output`, `moq_video_decoder_output`, `moq_consume_video`, and `moq_consume_audio` take `max_delay_us` instead of `max_age_us`.

### `stalled` is `enabled` (#4915)

`moq_consume_video_stalled` is `moq_consume_video_enabled`, with the meaning inverted.

### Routes have one cost (#4661)

`moq_route.cold` and `has_cold` are removed; drop them.

### `moq_publish_finish` is removed (#4152)

Close the broadcast with `moq_publish_close`.
To end its content first, call `moq_publish_catalog_finish`; the broadcast stays up until `moq_publish_close`.

### A decoded frame can fail on its own (#4094)

`moq_decode_video_frame` can return a conversion error for a single frame; it used to end the stream at the terminal callback.

## libmoq 0.5 -> 0.6

### One client config

The 41 `moq_client_*` setters are one zero-initializable `moq_client_config`, whose durations are `_us`, passed to `moq_session_connect` instead of `moq_client_connect`.

```c
moq_client_config config = { .tls_disable_verify = true };
moq_session_connect(url, url_len, &config, origin, 0, on_status, user_data);
```

### Publish split by kind

`moq_publish_media` is `moq_publish_audio`, `moq_publish_video`, or `moq_publish_container`, and the raw encoders are `moq_encode_audio*` / `moq_encode_video*`.
Formats are enums instead of strings.

```c
moq_audio_init init = { .format = MOQ_AUDIO_FORMAT_OPUS, .init = opus_head, .init_len = opus_head_len };
int track = moq_publish_audio(broadcast, &init);
```

### Announcement renames

| Before | After |
| --- | --- |
| `moq_announced` | `moq_announce_update` |
| `moq_origin_consume_announced` | `moq_origin_announced_broadcast` |
| `moq_broadcast_request_abort` | `moq_broadcast_request_reject` |
