---
title: C
description: libmoq, the stable C ABI over the Rust core
---

# C

[![GitHub release](https://img.shields.io/github/v/release/moq-dev/moq?filter=libmoq-v*\&label=libmoq)](https://github.com/moq-dev/moq/releases?q=libmoq)

`libmoq` exposes MoQ to C, C++, and any language with a C FFI through a
stable ABI: a generated `moq.h`, a static `libmoq.a` that links the whole Rust
runtime in, and a pkg-config file for its native link dependencies. The
[OBS plugin](/bin/obs) is built on it.

## Install

Each [`libmoq-v*` release](https://github.com/moq-dev/moq/releases?q=libmoq)
ships `moq-<version>-<target>.tar.gz` with `include/moq.h`, `lib/libmoq.a`, and
a pkg-config file listing the system libraries the archive needs. They are a
matched pair: compile against the header that came with the archive you link.
Targets: Linux x86\_64 and aarch64, macOS arm64, Windows x64.

```bash
export PKG_CONFIG_PATH="moq-$ver-$target/lib/pkgconfig"
cc app.c $(pkg-config --cflags --libs --static moq) -o app
```

From source: `cargo build --release -p libmoq` writes `target/release/libmoq.a`
and `target/include/moq.h`.

## Shape of the API

- **Handles and callbacks.** Every object is an integer handle; every async result arrives on a required `moq_status_callback` with a `void *user_data`. A status `> 0` is a live result, `0` a clean close, `< 0` an error, and the last two are terminal: libmoq never touches `user_data` again, so free it there. `*_cancel` only requests shutdown; the terminal callback still fires.
- **Errors.** Negative return codes named by `MOQ_ERROR_*` in `moq.h`, with `moq_error()` giving the reason for the last failure on the calling thread. Auth rejections (401, 403) have their own codes so you don't retry them. A protocol failure also fills `moq_error_protocol()` with the session or stream scope, the verbatim wire code, and a known kind; do not parse `moq_error()` for that.
- **Threading.** Any function from any thread. Raw publish calls block until the codec takes the frame, which paces a publisher.
- **Connection health.** `moq_session_stats()` reports available metrics with per-field validity flags. `moq_session_snapshot()` samples those metrics and the negotiated draft name together from the same connection. Its protocol string is backed by static storage. Both return an offline error between reconnects and leave the destination untouched. `moq_session_bandwidth()` mints an allocator over the send estimate; `moq_bandwidth_reserve` claims a share for an app-owned track, and `moq_encode_video` / `moq_encode_audio` take the same handle so the built-in video encoder follows the grant.
- **Raw playback.** Raw audio and video consumers start at the newest cached group when opened, so rebuilding a live decoder skips the retained backlog.
- **Raw decode output.** `moq_video_decoder_output` selects the decoded CPU pixel format (`MOQ_VIDEO_PIXEL_FORMAT_I420` or `_RGBA`) and target size (`width`/`height`, both zero for native; otherwise even and non-zero). Unknown formats and invalid sizes fail `moq_decode_video` before subscribing; accepted requests deliver exactly that layout or fail on the terminal callback.
- **Encoded video metadata.** `moq_video_init.hint` is a zero-initialized `moq_video_hint` with `has_*` flags for coded dimensions, bitrate (bits per second), frame rate, and latency preference. Hints seed a video codec track's catalog; detected dimensions take precedence.
- **Client config.** A zeroed `moq_client_config` means the defaults for every knob, which is what lets a new one be appended without disturbing callers. Fields cover protocol (`versions`), TLS (`tls_fingerprints`, `tls_roots`, `tls_cert`/`_key`, `tls_host_name`), transport (`bind`, `connect_timeout_us`, the Happy Eyeballs delays, `websocket_enabled`), and tuning (reconnect backoff, `quic_*`). Every duration is in microseconds. A knob whose default isn't zero carries a `has_*` flag, so setting `backoff_timeout_us = 0` needs `has_backoff_timeout = true` to mean "retry forever" rather than "use the default". `moq_client_defaults()` reports what a NULL config dials with.
- **Server.** `moq_server_listen` binds before it returns (a bad address or certificate fails there) and hands each incoming session to `on_request` as a request handle. Read `moq_session_request_path` and `_query` to route and authenticate, then `moq_session_request_accept` (a session handle, with origins like `moq_session_connect`) or `moq_session_request_reject` with an HTTP-style code (401 and 403 become the protocol's unauthorized close). An accepted session reports `1` once SETUP completes and never reconnects. `moq_server_addr` reports an ephemeral port and `moq_server_fingerprints` the hashes a client pins for a `tls_generate` certificate. `moq_server_close` stops listening; its terminal callback fires once the sockets are released.
- **Demand.** A watcher on a published track (`moq_publish_track_demand`, `moq_publish_media_demand`, `moq_encode_video_demand`, `moq_encode_audio_demand`) calls `on_demand` with `MOQ_DEMAND_USED` or `MOQ_DEMAND_UNUSED` right away and again on every change, so an encoder on a battery-powered device runs only while someone is watching. The first call is the current state, so a track that went unused before the watcher existed still reports it. `moq_publish_demand_cancel` stops it; the terminal callback still fires. A container has no single demand and is refused. Demand follows the last real subscriber: an origin that served the track drops its source copy on the unused edge and keeps only the finished groups it already cached warm for 30 seconds, so the cache linger does not delay the unused edge.
- **Requests.** `moq_publish_dynamic` serves subscriptions to tracks the broadcast never declared: each arrives as a request handle, read its name with `moq_track_request_name`, then `moq_track_request_accept` (a raw track handle), `moq_track_request_video` / `_audio` (the media handle `moq_publish_video` / `_audio` return), or `moq_track_request_abort` with an application code the subscriber sees. Without a live handler an unknown name is refused. `moq_publish_track_dynamic` does the same for fetches of groups a track no longer has cached, delivered as `moq_group_request_*` (`sequence`, `priority`, `frame_start`); `moq_group_request_accept` starts the producer at `frame_start` so written frames keep their group indices. Register it with `moq_track_request_dynamic` before accepting a track that was itself requested by a fetch, so that pending group survives the transition. Both handlers stop with `moq_publish_dynamic_cancel`.
- **Everything the bindings can do** ([list](/lib/#what-every-binding-can-do)): media publish and consume with the catalog managed for you, raw pixels and PCM with the codec inside (`moq_encode_video`, `moq_encode_audio`, and the `moq_decode_*` mirrors), raw tracks with timestamps and datagrams, JSON snapshot and stream tracks, group fetch, catalog sections, shared video properties, and stalled hints. The three advertising operations are `moq_origin_create_broadcast` (unannounced producer, invisible to everyone), `moq_publish_announce` / `moq_publish_unannounce` (exact-path advertisement), and `moq_origin_dynamic` (a claim over a path prefix and everything beneath it; `""` for everything). A route is a capability, not an inventory. `moq_origin_announced` takes a literal prefix and an optional relative pattern filter; `moq_announce_update.prefix` stays relative to the origin, while `captures` reports what each wildcard matched when `has_captures` is true. Paths with a `.`-prefixed segment below the prefix are [hidden](/concept/moq-lite#hidden-broadcasts); name the dot segment in `prefix` to list them.

```c
moq_client_config config;
memset(&config, 0, sizeof(config));   // zeroed means "the defaults", every field

moq_string fp = { hex_sha256, strlen(hex_sha256) };
config.tls_fingerprints = &fp;        // trust one self-signed relay
config.tls_fingerprints_len = 1;

int session = moq_session_connect(url, url_len, &config, origin, 0, on_status, user_data);
if (session < 0)
    return fail(moq_error());
```

## Connection stats

Every field in `moq_connection_stats` carries a matching `<field>_valid` flag,
`false` when the transport backend does not report it (a `false` flag is not the
same as zero). Native QUIC reports every metric; the browser WebTransport
reports few or none.

| Field | Unit | Meaning |
| --- | --- | --- |
| `rtt_us` | microseconds | Smoothed round-trip time. |
| `estimated_send_rate_bps` | bits per second | Send bandwidth from the congestion controller. |
| `estimated_recv_rate_bps` | bits per second | Receive bandwidth from MoQ PROBE. |
| `bytes_sent` | bytes | Total sent, including retransmissions and overhead. |
| `bytes_received` | bytes | Total received, including duplicates and overhead. |
| `bytes_lost` | bytes | Total lost, detected via retransmission or acknowledgement. |
| `packets_sent` | datagrams | Total datagrams sent. |
| `packets_received` | datagrams | Total datagrams received. |
| `packets_lost` | datagrams | Total datagrams detected as lost. |

The header is the reference; each function carries a doc comment. Source and
a worked example: [`rs/libmoq`](https://github.com/moq-dev/moq/tree/main/rs/libmoq),
API docs on [docs.rs/libmoq](https://docs.rs/libmoq).
