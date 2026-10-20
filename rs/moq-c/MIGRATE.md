# Migrating moq-c

How to update your code for each breaking change, newest first.

## Unreleased

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
