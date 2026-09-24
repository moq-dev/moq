# libmoq

C bindings for Media over QUIC.

## Building

```bash
cargo build --release
```

This will:

- Build the static library (`libmoq.a` on Unix-like systems, `moq.lib` on Windows)
- Generate the C header file at `target/include/moq.h`
- Generate the pkg-config file at `target/release/lib/pkgconfig/moq.pc`

There's also a [CMakeLists.txt](CMakeLists.txt) file that can be used to import/build the library.

## C API

The library exposes the following C functions, see [api.rs](src/api.rs) for full details:

```c
// Logging
int32_t moq_log_level(const char *level, uintptr_t level_len);

// Session
int32_t moq_session_connect(const char *url, uintptr_t url_len, const moq_client_config *config, uint32_t origin_publish, uint32_t origin_consume, moq_status_callback on_status, void *user_data);
moq_client_config moq_client_defaults(void);
int32_t moq_session_close(uint32_t session);
int32_t moq_session_bandwidth(uint32_t session);
int32_t moq_bandwidth_reserve(uint32_t bandwidth, uint32_t track, uint64_t max_bps);
int32_t moq_bandwidth_close(uint32_t bandwidth);
int32_t moq_reservation_grant(uint32_t reservation, uint64_t *bps, bool *present);
int32_t moq_reservation_update(uint32_t reservation, uint64_t max_bps);
int32_t moq_reservation_close(uint32_t reservation);

// Server
int32_t moq_server_listen(const moq_server_config *config, moq_status_callback on_request, void *user_data);
int32_t moq_server_addr(uint32_t server, moq_string *dst);
int32_t moq_server_fingerprints(uint32_t server, moq_string *dst, uintptr_t count);
int32_t moq_server_close(uint32_t server);
int32_t moq_session_request_path(uint32_t request, moq_string *dst);
int32_t moq_session_request_query(uint32_t request, moq_string *dst);
int32_t moq_session_request_accept(uint32_t request, uint32_t origin_publish, uint32_t origin_consume, moq_status_callback on_status, void *user_data);
int32_t moq_session_request_reject(uint32_t request, uint16_t code);
int32_t moq_session_request_free(uint32_t request);

// Origin
int32_t moq_origin_create(void);
int32_t moq_origin_close(uint32_t origin);
int32_t moq_origin_create_broadcast(uint32_t origin, const char *path, uintptr_t path_len);
int32_t moq_origin_request(uint32_t origin, const char *path, uintptr_t path_len, moq_status_callback on_broadcast, void *user_data);
int32_t moq_origin_request_cancel(uint32_t task);
int32_t moq_origin_announced_broadcast(uint32_t origin, const char *path, uintptr_t path_len, moq_status_callback on_broadcast, void *user_data);
int32_t moq_origin_announced_broadcast_cancel(uint32_t task);
int32_t moq_origin_announced(uint32_t origin, const char *prefix, uintptr_t prefix_len, const char *filter, uintptr_t filter_len, moq_status_callback on_announce, void *user_data);
int32_t moq_origin_announced_info(uint32_t announced, moq_announce_update *dst);
int32_t moq_origin_announced_free(uint32_t announced);
int32_t moq_origin_announced_cancel(uint32_t announced);
// filter is relative to the literal prefix, or NULL for **. Updates stay relative to the origin.

// Publishing
int32_t moq_publish_announce(uint32_t broadcast, const moq_route *route);
int32_t moq_publish_unannounce(uint32_t broadcast);
int32_t moq_publish_finish(uint32_t broadcast);
int32_t moq_publish_audio(uint32_t broadcast, const moq_audio_init *config);
int32_t moq_publish_video(uint32_t broadcast, const moq_video_init *config);
int32_t moq_publish_container(uint32_t broadcast, const moq_container_init *config);
int32_t moq_publish_container_write(uint32_t container, const uint8_t *payload, uintptr_t payload_size);
int32_t moq_publish_container_finish(uint32_t container);
int32_t moq_publish_media_finish(uint32_t media);
int32_t moq_publish_media_frame(uint32_t media, const uint8_t *payload, uintptr_t payload_size, uint64_t timestamp_us);
int32_t moq_publish_track(uint32_t broadcast, const char *name, uintptr_t name_len, const moq_track_info *info);
int32_t moq_publish_track_group(uint32_t track);
int32_t moq_publish_track_frame(uint32_t track, const uint8_t *payload, uintptr_t payload_size, uint64_t timestamp_us);
int32_t moq_publish_group_frame(uint32_t group, const uint8_t *payload, uintptr_t payload_size, uint64_t timestamp_us);
int32_t moq_publish_group_finish(uint32_t group);
int32_t moq_publish_track_finish(uint32_t track);

// Publishing: Demand
int32_t moq_publish_track_demand(uint32_t track, moq_status_callback on_demand, void *user_data);
int32_t moq_publish_media_demand(uint32_t media, moq_status_callback on_demand, void *user_data);
int32_t moq_encode_video_demand(uint32_t producer, moq_status_callback on_demand, void *user_data);
int32_t moq_encode_audio_demand(uint32_t producer, moq_status_callback on_demand, void *user_data);
int32_t moq_publish_demand_cancel(uint32_t watcher);

// Publishing: Requests
int32_t moq_publish_dynamic(uint32_t broadcast, moq_status_callback on_request, void *user_data);
int32_t moq_publish_track_dynamic(uint32_t track, moq_status_callback on_group, void *user_data);
int32_t moq_track_request_dynamic(uint32_t request, moq_status_callback on_group, void *user_data);
int32_t moq_publish_dynamic_cancel(uint32_t dynamic);
int32_t moq_track_request_name(uint32_t request, moq_string *dst);
int32_t moq_track_request_accept(uint32_t request, const moq_track_info *info);
int32_t moq_track_request_video(uint32_t request, const moq_video_init *config);
int32_t moq_track_request_audio(uint32_t request, const moq_audio_init *config);
int32_t moq_track_request_abort(uint32_t request, uint16_t error_code);
int32_t moq_track_request_free(uint32_t request);
int32_t moq_group_request_sequence(uint32_t request, uint64_t *dst);
int32_t moq_group_request_priority(uint32_t request, uint8_t *dst);
int32_t moq_group_request_frame_start(uint32_t request, uint64_t *dst);
int32_t moq_group_request_accept(uint32_t request);
int32_t moq_group_request_abort(uint32_t request, uint16_t error_code);
int32_t moq_group_request_free(uint32_t request);

// Consuming
int32_t moq_consume_close(uint32_t consume);

// Consuming: Catalog
int32_t moq_consume_catalog(uint32_t broadcast, moq_status_callback on_catalog, void *user_data);
int32_t moq_consume_catalog_cancel(uint32_t catalog);
int32_t moq_consume_catalog_free(uint32_t catalog);
int32_t moq_consume_video_config(uint32_t catalog, uint32_t index, moq_video_config *dst);
int32_t moq_consume_video_stalled(uint32_t catalog, uint32_t index, bool *dst);
int32_t moq_consume_audio_config(uint32_t catalog, uint32_t index, moq_audio_config *dst);

// Consuming: Video
int32_t moq_consume_video(uint32_t catalog, uint32_t index, uint64_t max_age_us, moq_status_callback on_frame, void *user_data);
int32_t moq_consume_video_cancel(uint32_t track);

// Consuming: Audio
int32_t moq_consume_audio(uint32_t catalog, uint32_t index, uint64_t max_age_us, moq_status_callback on_frame, void *user_data);
int32_t moq_consume_audio_cancel(uint32_t track);

// Consuming: Frames
int32_t moq_consume_frame(uint32_t frame, moq_frame *dst);
int32_t moq_consume_frame_free(uint32_t frame);
int32_t moq_consume_track(uint32_t broadcast, const char *name, uintptr_t name_len, const moq_subscription *subscription, moq_status_callback on_frame, void *user_data);
int32_t moq_consume_track_frame(uint32_t frame, moq_frame *dst);
int32_t moq_consume_track_frame_free(uint32_t frame);
int32_t moq_consume_track_cancel(uint32_t track);
```

Raw track frames use the same `moq_frame` record as media frames. Use
`moq_publish_track_frame` or `moq_publish_group_frame` to provide microsecond
presentation timestamps; `moq_consume_track_frame` returns that timestamp in
`moq_frame.timestamp_us`. libmoq creates raw tracks with a microsecond
timescale, matching the C ABI's timestamp units.
