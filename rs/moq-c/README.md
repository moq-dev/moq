# moq-c

C bindings for Media over QUIC.

## Building

```bash
cargo build --release
```

This will:

- Build the static library (`libmoq.a` on Unix-like systems, `moq.lib` on Windows)
- Generate the C header file at `$OUT_DIR/include/moq.h`

`OUT_DIR` is the build script's hashed output directory, which
`cargo build --message-format=json` reports as `out_dir` on the
`build-script-executed` message for moq-c.
The pkg-config file (`moq-c.pc.in`) is rendered only when packaging (`nix build .#moq-c`
and the release tarballs), since its paths assume `lib/libmoq.a` beside `lib/pkgconfig/`.

There's also a [CMakeLists.txt](CMakeLists.txt) file that can be used to import/build the library.

## C API

The library exposes the following C functions, see [api.rs](src/api.rs) for full details:

```c
// Logging
int32_t moq_log_level(const char *level, uintptr_t level_len);

// Session
int32_t moq_session_connect(const char *url, uintptr_t url_len, const moq_client_config *config, uint32_t origin_publish, uint32_t origin_consume, moq_state_callback on_state, void *user_data);
moq_client_config moq_client_defaults(void);
int32_t moq_session_close(uint32_t session);
int32_t moq_session_allocator(uint32_t session);
int32_t moq_allocator_reserve(uint32_t allocator, uint32_t track, uint64_t max_bps);
int32_t moq_allocator_close(uint32_t allocator);
int32_t moq_reservation_rate(uint32_t reservation, uint64_t *bps, bool *present);
int32_t moq_reservation_update(uint32_t reservation, uint64_t max_bps);
int32_t moq_reservation_close(uint32_t reservation);
int32_t moq_session_viewers(uint32_t session);
int32_t moq_viewers_watch(uint32_t viewers, uint64_t max_bps);
int32_t moq_viewers_set_target(uint32_t viewers, uint64_t target_bps, bool present);
int32_t moq_viewers_close(uint32_t viewers);
int32_t moq_viewer_rate(uint32_t viewer, uint64_t *bps, bool *present);
int32_t moq_viewer_update(uint32_t viewer, uint64_t max_bps);
int32_t moq_viewer_close(uint32_t viewer);

// Session: Auth (a dialed session's tokens, and the grant the peer answers with)
int32_t moq_session_tokens_set(uint32_t session, const moq_string *tokens, uintptr_t tokens_len);
int32_t moq_session_tokens_answered(uint32_t session, moq_status_callback on_answer, void *user_data);
int32_t moq_session_grants(uint32_t session, moq_status_callback on_grant, void *user_data);
int32_t moq_session_grants_cancel(uint32_t watcher);
int32_t moq_grant_info(uint32_t grant, moq_grant *dst);
int32_t moq_grant_free(uint32_t grant);
int32_t moq_session_credential(uint32_t session, const uint8_t *token, uintptr_t token_len);
int32_t moq_credential_answered(uint32_t credential, moq_status_callback on_answer, void *user_data);
int32_t moq_credential_free(uint32_t credential);

// Server
int32_t moq_server_listen(const moq_server_config *config, moq_status_callback on_request, void *user_data);
int32_t moq_server_addr(uint32_t server, moq_string *dst);
int32_t moq_server_fingerprints(uint32_t server, moq_string *dst, uintptr_t count);
int32_t moq_server_close(uint32_t server);
int32_t moq_session_request_path(uint32_t request, moq_string *dst);
int32_t moq_session_request_query(uint32_t request, moq_string *dst);
int32_t moq_session_request_accept(uint32_t request, uint32_t origin_publish, uint32_t origin_consume, moq_state_callback on_state, void *user_data);
int32_t moq_session_request_reject(uint32_t request, uint16_t code);
int32_t moq_session_request_free(uint32_t request);

// Origin
int32_t moq_origin_create(void);
int32_t moq_origin_close(uint32_t origin);
int32_t moq_origin_request(uint32_t origin, const char *path, uintptr_t path_len, uint32_t instance, moq_status_callback on_broadcast, void *user_data);
int32_t moq_origin_request_cancel(uint32_t task);
int32_t moq_origin_follow(uint32_t origin, const char *path, uintptr_t path_len, uint32_t instance, moq_follow_callback on_follow, void *user_data);
int32_t moq_origin_follow_cancel(uint32_t task);
int32_t moq_origin_announced(uint32_t origin, const moq_announce_config *config, moq_status_callback on_announce, void *user_data);
int32_t moq_origin_announced_info(uint32_t announced, moq_announce_event *dst);
int32_t moq_origin_announced_free(uint32_t announced);
int32_t moq_origin_announced_cancel(uint32_t announced);
// config (NULL for everything): filter is relative to the literal prefix, or NULL for **. hidden also lists `.`-named paths. Updates stay relative to the origin.

// Instances (0 means none: unpinned, or a fresh one in route options; a route's or claim's is the caller's to free, an announcement's lives with it)
int32_t moq_instance_parse(const char *text, uintptr_t text_len);
int32_t moq_instance_text(uint32_t instance, moq_string *dst);
int32_t moq_instance_minted(uint32_t instance);
int32_t moq_instance_equal(uint32_t a, uint32_t b);
int32_t moq_instance_free(uint32_t instance);

// Publishing: Routes (freeing one retracts it; moq_publish_close on its broadcast ends both)
int32_t moq_origin_route(uint32_t origin, const char *path, uintptr_t path_len, const moq_route_options *options);
// options (NULL for the default): hidden until announced, a cost, and an optional stable instance with its epoch. Hops are the origin's to add.
int32_t moq_route_broadcast(uint32_t route);
int32_t moq_route_instance(uint32_t route);
int32_t moq_route_announce(uint32_t route);
int32_t moq_route_withdraw(uint32_t route);
int32_t moq_route_set_cost(uint32_t route, uint64_t cost);
int32_t moq_route_set_auth(uint32_t route, uint32_t credential);
int32_t moq_route_free(uint32_t route);

// Publishing: Claims (a prefix answered on demand; freeing one retracts it)
int32_t moq_origin_claim(uint32_t origin, const char *prefix, uintptr_t prefix_len, const moq_route_options *options, moq_status_callback on_request, void *user_data);
int32_t moq_claim_instance(uint32_t claim);
int32_t moq_claim_announce(uint32_t claim);
int32_t moq_claim_withdraw(uint32_t claim);
int32_t moq_claim_set_cost(uint32_t claim, uint64_t cost);
int32_t moq_claim_set_auth(uint32_t claim, uint32_t credential);
int32_t moq_claim_free(uint32_t claim);
int32_t moq_broadcast_request_path(uint32_t request, moq_string *dst);
int32_t moq_broadcast_request_serve(uint32_t request, uint32_t broadcast);
int32_t moq_broadcast_request_route(uint32_t request, const moq_route_options *options);
int32_t moq_broadcast_request_reject(uint32_t request, uint16_t error_code);
int32_t moq_broadcast_request_free(uint32_t request);

// Publishing
int32_t moq_publish_close(uint32_t broadcast);
int32_t moq_publish_audio(uint32_t broadcast, const moq_audio_init *config);
int32_t moq_publish_video(uint32_t broadcast, const moq_video_init *config);
int32_t moq_publish_container(uint32_t broadcast, const moq_container_init *config);
int32_t moq_publish_container_write(uint32_t container, const uint8_t *payload, uintptr_t payload_size);
int32_t moq_publish_container_finish(uint32_t container);
int32_t moq_publish_media_finish(uint32_t media);
int32_t moq_publish_media_frame(uint32_t media, const uint8_t *payload, uintptr_t payload_size, moq_timestamp timestamp);
int32_t moq_publish_track(uint32_t broadcast, const char *name, uintptr_t name_len, const moq_track_info *info);
int32_t moq_publish_track_group(uint32_t track);
int32_t moq_publish_track_frame(uint32_t track, const uint8_t *payload, uintptr_t payload_size, const moq_timestamp *timestamp);
int32_t moq_publish_group_frame(uint32_t group, const uint8_t *payload, uintptr_t payload_size, const moq_timestamp *timestamp);
int32_t moq_publish_group_sequence(uint32_t group, uint64_t *dst);
int32_t moq_publish_group_finish(uint32_t group);
int32_t moq_publish_group_ready(uint32_t group, uint64_t size, moq_status_callback on_ready, void *user_data);
int32_t moq_publish_track_finish(uint32_t track);
int32_t moq_publish_track_largest(uint32_t track, uint64_t *group, uint64_t *frame, bool *present);
int32_t moq_publish_timebase(uint32_t broadcast);
int32_t moq_publish_timebase_free(uint32_t timebase);
// moq_container_init.timebase (0 for none) shares one source clock across containers.

// Publishing: Demand
int32_t moq_publish_track_demand(uint32_t track, moq_status_callback on_demand, void *user_data);
int32_t moq_publish_media_demand(uint32_t media, moq_status_callback on_demand, void *user_data);
int32_t moq_encode_video_demand(uint32_t producer, moq_status_callback on_demand, void *user_data);
int32_t moq_encode_audio_demand(uint32_t producer, moq_status_callback on_demand, void *user_data);
int32_t moq_publish_demand_cancel(uint32_t watcher);
int32_t moq_publish_demand_wanted(uint32_t watcher, uint64_t *group, bool *present);

// Publishing: Requests
int32_t moq_publish_requests(uint32_t broadcast, moq_status_callback on_request, void *user_data);
int32_t moq_publish_track_requests(uint32_t track, moq_status_callback on_group, void *user_data);
int32_t moq_track_request_requests(uint32_t request, moq_status_callback on_group, void *user_data);
int32_t moq_publish_requests_cancel(uint32_t dynamic);
int32_t moq_track_request_name(uint32_t request, moq_string *dst);
int32_t moq_track_request_accept(uint32_t request, const moq_track_info *info);
int32_t moq_track_request_video(uint32_t request, const moq_video_init *config);
int32_t moq_track_request_audio(uint32_t request, const moq_audio_init *config);
int32_t moq_track_request_abort(uint32_t request, uint16_t error_code);
int32_t moq_track_request_free(uint32_t request);
int32_t moq_group_request_sequence(uint32_t request, uint64_t *dst);
int32_t moq_group_request_priority(uint32_t request, int8_t *dst);
int32_t moq_group_request_frame_start(uint32_t request, uint64_t *dst);
int32_t moq_group_request_accept(uint32_t request);
int32_t moq_group_request_abort(uint32_t request, uint16_t error_code);
int32_t moq_group_request_free(uint32_t request);

// Consuming
int32_t moq_consume_close(uint32_t consume);
int32_t moq_consume_closed(uint32_t broadcast, moq_status_callback on_closed, void *user_data);
int32_t moq_consume_closed_cancel(uint32_t watcher);

// Consuming: Catalog
int32_t moq_consume_catalog(uint32_t broadcast, moq_status_callback on_catalog, void *user_data);
int32_t moq_consume_catalog_cancel(uint32_t catalog);
int32_t moq_consume_catalog_free(uint32_t catalog);
int32_t moq_consume_video_config(uint32_t catalog, uint32_t index, moq_video_config *dst);
int32_t moq_consume_video_enabled(uint32_t catalog, uint32_t index, bool *dst);
int32_t moq_consume_audio_config(uint32_t catalog, uint32_t index, moq_audio_config *dst);
int32_t moq_consume_audio_enabled(uint32_t catalog, uint32_t index, bool *dst);

// Consuming: Video
int32_t moq_consume_video(uint32_t catalog, uint32_t index, const moq_delay *delay, moq_status_callback on_frame, void *user_data);
int32_t moq_consume_video_cancel(uint32_t track);

// Consuming: Audio
int32_t moq_consume_audio(uint32_t catalog, uint32_t index, const moq_delay *delay, moq_status_callback on_frame, void *user_data);
int32_t moq_consume_audio_cancel(uint32_t track);

// Consuming: Frames
int32_t moq_consume_read(uint32_t read, moq_media_read *dst);
int32_t moq_consume_read_free(uint32_t read);
moq_subscription moq_subscription_defaults(void);
int32_t moq_consume_track(uint32_t broadcast, const char *name, uintptr_t name_len, const moq_subscription *subscription, moq_status_callback on_frame, void *user_data);
int32_t moq_consume_track_frame(uint32_t frame, moq_frame *dst);
int32_t moq_consume_track_frame_free(uint32_t frame);
int32_t moq_consume_track_cancel(uint32_t track);

// Sockets: two-way sessions between endpoints sharing an origin
int32_t moq_socket_create(uint32_t origin, const char *prefix, uintptr_t prefix_len, const char *id, uintptr_t id_len);
int32_t moq_socket_close(uint32_t endpoint);
int32_t moq_socket_dial_tcp(uint32_t endpoint, const char *peer, uintptr_t peer_len, moq_status_callback on_stream, void *user_data);
int32_t moq_socket_dial_track(uint32_t endpoint, const char *peer, uintptr_t peer_len, const moq_track_info *info, moq_status_callback on_stream, void *user_data);
int32_t moq_socket_accept_tcp(uint32_t endpoint, moq_status_callback on_incoming, void *user_data);
int32_t moq_socket_accept_track(uint32_t endpoint, moq_status_callback on_incoming, void *user_data);
int32_t moq_socket_cancel(uint32_t task);
int32_t moq_socket_incoming_info(uint32_t incoming, moq_string *peer, moq_string *session);
int32_t moq_socket_incoming_accept(uint32_t incoming, const moq_track_info *info, moq_status_callback on_stream, void *user_data);
int32_t moq_socket_incoming_reject(uint32_t incoming);
int32_t moq_socket_stream_info(uint32_t stream, moq_string *peer, moq_string *session);
int32_t moq_socket_stream_writer(uint32_t stream);  // a group (tcp) or raw track (track) handle
int32_t moq_socket_stream_reader(uint32_t stream, moq_status_callback on_frame, void *user_data);  // read like moq_consume_track
int32_t moq_socket_stream_free(uint32_t stream);

// Codecs without a broadcast: encode, then publish or store the packets yourself
int32_t moq_audio_encoder_create(const moq_audio_encoder_output *output);
int32_t moq_audio_encoder_frame_size(uint32_t encoder);
int32_t moq_audio_encoder_init(uint32_t encoder, moq_audio_init *dst);
int32_t moq_audio_encoder_bitrate(uint32_t encoder, uint64_t bitrate);
int32_t moq_audio_encoder_encode(uint32_t encoder, const float *pcm, uintptr_t pcm_len);
int32_t moq_audio_encoder_receive(uint32_t encoder, moq_audio_packet *dst);
int32_t moq_audio_encoder_flush(uint32_t encoder);
int32_t moq_audio_encoder_finish(uint32_t encoder, const float *pcm, uintptr_t pcm_len, uint64_t *discard_padding);
int32_t moq_audio_encoder_packet(uint32_t encoder, uint32_t index, moq_audio_packet *dst);
int32_t moq_audio_encoder_free(uint32_t encoder);
int32_t moq_video_encoder_create(const moq_video_encoder_input *input, const moq_video_encoder_output *output);
int32_t moq_video_encoder_encode(uint32_t encoder, const moq_video_encoder_frame *frame);
int32_t moq_video_encoder_receive(uint32_t encoder, moq_frame *dst);
int32_t moq_video_encoder_cut(uint32_t encoder);
int32_t moq_video_encoder_bitrate(uint32_t encoder, uint64_t bitrate);
int32_t moq_video_encoder_flush(uint32_t encoder);
int32_t moq_video_encoder_free(uint32_t encoder);
```

Raw track frames use the same `moq_frame` record as media frames. Timestamps
are exact `moq_timestamp` records, `value` units at `scale` units per second.
A scale-less track has no time, so there `has_scale` is false and `value` is
the frame's group key (a datagram's sequence). Every write takes a non-NULL
timestamp: `moq_publish_track_frame` and `moq_publish_group_frame` convert
theirs to the track's timescale, a microsecond one unless
`moq_track_info.timescale` sets another, and `moq_publish_group_sequence` reads
the key a scale-less group's frames carry. `moq_consume_track_frame` points
`moq_frame.timestamp` at the timestamp read, never NULL, valid until the frame
is freed.

A zeroed `moq_track_info`, like a NULL one, advertises a zero max age, how long
a superseded group stays fresh, so only the live edge is served. Set
`max_age_us` for a longer age, or `max_age_unlimited` for no publisher limit,
which ignores `max_age_us`. Zero-initialize the struct
(`moq_track_info info = {0};`) before assigning members, so every flag starts
false.

JSON window pushes and pops take a required `const moq_timestamp *timestamp`,
like snapshot updates and stream appends. It is when the edit occurred,
converted to the track's milliseconds; stamp it from `moq_publish_catalog_now`.
A NULL timestamp fails with `MOQ_ERROR_INVALID_POINTER`.

```c
int32_t moq_publish_json_window_push(uint32_t window, const char *value, uintptr_t value_len, const moq_timestamp *timestamp);
int32_t moq_publish_json_window_pop(uint32_t window, uint64_t count, const moq_timestamp *timestamp);
```

### Pre-encoded simulcast

Publish video renditions from lowest to highest, passing the next lower media
handle in `moq_video_init.lower` (0 for the lowest). This orders delivery so
higher renditions yield to lower ones. `moq_publish_media_set_enabled` updates
the catalog and marks a discontinuity when disabling; resume on a keyframe.
Audio importers do not support this toggle.

Reserve each media track with `moq_allocator_reserve_media`. Create a
`moq_rate_control_create` handle bound to the reservation, with `moq_rate_policy.max_bps` and, for higher
rungs, `has_lower` and `lower_bps`. Read its initial `moq_rate_control_target`,
then call `moq_rate_control_update` on every frame,
including while disabled. It returns 0 or a negative error; `target.changed`
is true for a new target and false to hold the last one. Apply the target's `bitrate_bps`
to your encoder and `enabled` through `moq_publish_media_set_enabled`.
The shared controller drops immediately, ramps raises, ignores small changes,
and holds without an estimate. A rung disables only with a probing session,
which on native transports requires moq-quic with BBR3, so bandwidth can recover.
Close controls with `moq_rate_control_close` and reservations with
`moq_reservation_close`; neither owns or closes the media track.
