# Migrating moq-c

How to update your code for each breaking change, newest first.

## Unreleased

### Receive estimates come from `moq_session_recv_bandwidth` (#5777)

`moq_connection_stats` drops `estimated_recv_rate_bps` and `estimated_recv_rate_valid`, which changes its ABI.
Hold the consumer `moq_session_recv_bandwidth` returns to request PROBE, read it with `moq_bandwidth_consumer_peek` or wait with `moq_bandwidth_consumer_changed`, and release it with `moq_bandwidth_consumer_close`.

```c
// Before
moq_connection_stats stats;
moq_session_stats(session, &stats);
if (stats.estimated_recv_rate_valid) use(stats.estimated_recv_rate_bps);
// After
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

```cmake
# Before
find_package(moq REQUIRED)
target_link_libraries(app PRIVATE moq)
# After
find_package(moq-c REQUIRED)
target_link_libraries(app PRIVATE moq::c)
```

### Background panics are reported (#5562)

A panic in a background task reports `MOQ_ERROR_PANIC`, and other handles keep running.

### Announcements carry a kind (#4266, #4384, #5087)

`moq_announce_update` is `moq_announce_event`, and its `active` flag is a `kind`: `MOQ_ANNOUNCE_KIND_START`, `_UPDATE`, `_END`, or `_RESTART`.
`_RESTART` means another publisher instance now serves the prefix: request the path again.

```c
// Before
moq_announce_update info;
moq_origin_announced_info(id, &info);
if (info.active) subscribe(info.prefix, info.prefix_len);
// After
moq_announce_event info;
moq_origin_announced_info(id, &info);
if (info.kind == MOQ_ANNOUNCE_KIND_START || info.kind == MOQ_ANNOUNCE_KIND_RESTART) subscribe(info.prefix, info.prefix_len);
```

### `moq_origin_announced` takes a config (#4111)

The prefix and filter arguments moved into a `moq_announce_config`; pass `NULL` to receive everything.

```c
// Before
moq_origin_announced(origin, prefix, prefix_len, NULL, 0, on_announce, user_data);
// After
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

```c
// Before
moq_subscription sub = { .max_age_us = 500000 };
// After
moq_subscription sub = { .max_delay_us = 500000 };
```

### `stalled` is `enabled` (#4915)

`moq_consume_video_stalled` is `moq_consume_video_enabled`, with the meaning inverted.

```c
// Before
bool stalled;
moq_consume_video_stalled(catalog, index, &stalled);
// After
bool enabled;
moq_consume_video_enabled(catalog, index, &enabled);
```

### Routes have one cost (#4661)

`moq_route.cold` and `has_cold` are removed.

```c
// Before
moq_route route = { .cost = 10, .cold = 20, .has_cold = true };
// After
moq_route route = { .cost = 10 };
```

### `moq_publish_finish` is removed (#4152)

Close the broadcast with `moq_publish_close`.

```c
// Before
moq_publish_finish(broadcast);
// After
moq_publish_close(broadcast);
```

### A decoded frame can fail on its own (#4094)

`moq_decode_video_frame` can return a conversion error for a single frame; it used to end the stream at the terminal callback.

## libmoq 0.5 -> 0.6

### One client config

The 41 `moq_client_*` setters are one zero-initializable `moq_client_config`, whose durations are `_us`.

```c
// Before
int client = moq_client_create();
moq_client_set_tls_disable_verify(client, true);
moq_client_connect(url, url_len, client, origin, 0, on_status, user_data);
// After
moq_client_config config = { .tls_disable_verify = true };
moq_session_connect(url, url_len, &config, origin, 0, on_status, user_data);
```

### Publish split by kind

`moq_publish_media` is `moq_publish_audio`, `moq_publish_video`, or `moq_publish_container`, and the raw encoders are `moq_encode_audio*` / `moq_encode_video*`.
Formats are enums instead of strings.

```c
// Before
int track = moq_publish_media(broadcast, "opus", 4, opus_head, opus_head_len);
// After
moq_audio_init init = { .format = MOQ_AUDIO_FORMAT_OPUS, .init = opus_head, .init_len = opus_head_len };
int track = moq_publish_audio(broadcast, &init);
```

### Announcement renames

| Before | After |
| --- | --- |
| `moq_announced` | `moq_announce_update` |
| `moq_origin_consume_announced` | `moq_origin_announced_broadcast` |
| `moq_broadcast_request_abort` | `moq_broadcast_request_reject` |
