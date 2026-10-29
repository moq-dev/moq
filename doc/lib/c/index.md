---
title: C
description: moq-c, the stable C ABI over the Rust core
---

# C

[![GitHub release](https://img.shields.io/github/v/release/moq-dev/moq?filter=moq-c-v*\&label=moq-c)](https://github.com/moq-dev/moq/releases?q=moq-c)

`moq-c` exposes MoQ to C and any language with a C FFI through a
stable ABI: a generated `moq.h`, a static `libmoq.a` that links the whole Rust
runtime in, and a pkg-config file for its native link dependencies. Releases
through 0.6 were named `libmoq`. C++ projects should prefer the
[C++ package](/lib/cpp/) (`moq-cpp`): the same core as RAII objects and
futures instead of handles and callbacks.

## Install

Each [`moq-c-v*` release](https://github.com/moq-dev/moq/releases?q=moq-c)
ships `moq-c-<version>-<target>.tar.gz` with `include/moq.h`, `lib/libmoq.a`, and
a pkg-config file listing the system libraries the archive needs. They are a
matched pair: compile against the header that came with the archive you link.
Targets: Linux x86\_64 and aarch64, macOS arm64, Windows x64.

```bash
export PKG_CONFIG_PATH="moq-c-$ver-$target/lib/pkgconfig"
cc app.c $(pkg-config --cflags --libs --static moq-c) -o app
```

With CMake, point `CMAKE_PREFIX_PATH` at the extracted archive, then
`find_package(moq-c)` and link `moq::c`.

From source, `add_subdirectory(rs/moq-c)` in CMake gives the same `moq::c`
target, and `nix build .#moq-c` produces the release layout. A bare
`cargo build -p moq-c` leaves `moq.h` in a hashed `OUT_DIR`, writes no
pkg-config file, and omits the OpenH264 software fallback unless given
`--features openh264`.

`moq-c` and the C++ package each link in their own copy of the Rust core, so a
program links one of the two, never both.

Encoded media callbacks from `moq_consume_audio` and `moq_consume_video` deliver read
handles. Inspect each with `moq_consume_read` and match `moq_media_read.kind`, using the
same frame/gap/end tags as decoded audio and video. Release every kind with
`moq_consume_read_free`; frame payload pointers borrow that handle. A gap breaks
continuity. An encoder end is not callback EOF: frames before its endpoint are the old
encoder's drain, and the first at or past it starts fresh. Callback 0 ends the stream.

## Connect

```c
moq_client_config config;
memset(&config, 0, sizeof(config));   // zeroed means "the defaults", every field

moq_string fp = { hex_sha256, strlen(hex_sha256) };
config.tls_fingerprints = &fp;        // trust one self-signed relay
config.tls_fingerprints_len = 1;

int session = moq_session_connect(url, url_len, &config, origin, 0, on_state, user_data);
if (session < 0)
    return fail(moq_error());
```

## Publish

```c
const char *path = "my-stream.hang";
uint32_t broadcast;
int publication = moq_origin_create_broadcast(origin, path, strlen(path), &broadcast);
if (publication < 0)
    return fail(moq_error());
// Fill tracks on broadcast here; announce on the publication.

char epoch[MOQ_EPOCH_LEN];
moq_mint_epoch(epoch, sizeof(epoch)); // a fresh epoch per run
moq_announce_route route;
memset(&route, 0, sizeof(route));     // zeroed means cost 0 and no epoch
route.epoch = epoch;
route.epoch_len = sizeof(epoch);
if (moq_publication_announce((uint32_t)publication, &route) < 0)
    return fail(moq_error());
```

A route carries only a cost and an epoch; relays add the hops. Mint a fresh epoch each
time the publisher starts, so viewers see a restart (`MOQ_ANNOUNCE_KIND_RESTART`) instead of
a stalled broadcast. Redundant publishers of the same bytes share one.

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Publication track names stay separate from codec settings. For simulcast, give each rendition the next one down as the `lower` handle in its `moq_video_encode_options`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) is
here, from media publish and consume to raw pixels and PCM with the codec
inside. The header is the reference: each function carries a doc comment.
[Layers](/lib/layers) maps each wrapped Rust layer to its function prefixes.

- **Handles and callbacks.** Every object is an integer handle, and every async result arrives on a `moq_status_callback` with your `void *user_data`. A status `> 0` is a live result, `0` a clean close, and `< 0` an error. The last two are terminal: moq-c never touches `user_data` again, so free it there. `*_close` and `*_cancel` only request shutdown; the terminal callback still fires. A session reports on a `moq_state_callback` instead: each `moq_connection_state` carries its `phase`, `connects`, the negative `failure` of the latest attempt, and the live `protocol`, and the `MOQ_CONNECTION_PHASE_CLOSED` snapshot is terminal, delivered after the handle is released. `moq_session_close()` drains finished tracks for dialed and accepted sessions; wait for that terminal snapshot to learn the result.
- **Errors.** Negative return codes are named by `MOQ_ERROR_*`, and `moq_error()` gives the reason on the calling thread. Auth rejections have their own codes so you don't retry them. For the peer's protocol code use `moq_error_protocol()`; do not parse `moq_error()`. A panic in a binding background task reports `MOQ_ERROR_PANIC` through its terminal callback; unrelated objects keep running. A session closed because its credential expired has kind `MOQ_PROTOCOL_KIND_EXPIRED`, and a request refused for an expired token returns `MOQ_ERROR_EXPIRED`; refresh the token before retrying. A catalog that needs a newer moq-hang draft than this build reads returns `MOQ_ERROR_UNSUPPORTED_CATALOG`; upgrade the library to read it. A request-cap closure has kind `MOQ_PROTOCOL_KIND_TOO_MANY_REQUESTS`; reduce the workload or adjust the relay's [session limits](/bin/relay/config#session-and-peer) before reconnecting.
- **Tokens.** Set `moq_client_config.tokens` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release. To replace expiring tokens, close the session and connect again.
- **Threading.** Call any function from any thread. Raw publish calls block until the codec takes the frame, which paces a publisher.
- **Zeroed config is the default.** A zeroed `moq_client_config` means the default for every field, so new fields don't break callers. A field whose default isn't zero has a `has_*` flag: `backoff_timeout_us = 0` means "retry forever" only with `has_backoff_timeout = true`.
- **Compressed tracks.** `moq_track_info.compress` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames.
- **fMP4 import.** `MOQ_CONTAINER_FORMAT_FMP4` splits each fragment into one frame per sample.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `moq_publish_media_cut`: after every frame, or at a segment cadence to align with video.
- **Ending cleanly.** When the input ends, finish every track, then call `moq_publish_catalog_finish()` to finish the catalog, which ends the content. The broadcast stays announced and keeps serving the finished tracks, so a subscriber still catching up can drain them, until `moq_publish_close()` takes it offline. How long to keep it up is yours to decide. A track still open at the close ends with `UNROUTABLE`, which readers take as the publisher leaving, and its handle is released.
- **Timeline breaks.** On a seek or pause, call `moq_publish_media_discontinuity`, then keep timestamps moving forward and resume video on a keyframe.
- **JSON values.** `moq_consume_json_value` fills `moq_json_value` with the document and its frame's `timestamp_us`, rounded down to a whole microsecond; `timestamp_present` is false on an untimed track. A snapshot consumer's callback fires for each state in order, but a group superseded before it is read is skipped.
- **Decoded reads.** `moq_decode_audio` and `moq_decode_video` deliver read ids in order: a frame, a gap where media was lost, or an end where the publisher stopped its encoder. Check `kind` on what `moq_decode_audio_read` or `moq_decode_video_read` writes. Free each id with `moq_decode_audio_read_free` or `moq_decode_video_read_free` promptly: a held video frame holds a decoder buffer, and the decoder stalls.
- **Stats.** `moq_session_stats()` fills `moq_connection_stats` with `rtt_us`, `estimated_send_rate_bps`, and the byte and packet counters (`bytes_sent`, `bytes_received`, `bytes_lost`, `packets_sent`, `packets_received`, `packets_lost`). Each has a `<field>_valid` flag, false when the transport does not report it, which is not the same as zero. `moq_session_snapshot()` adds the negotiated `protocol`. A failed dial or dropped connection reaches the session's `on_state` with its `failure` code, and `moq_error()` gives the reason during that callback. Hold the handle returned by `moq_session_recv_bandwidth()` to request receive estimates. `moq_bandwidth_consumer_peek()` reads the estimate and `moq_bandwidth_consumer_changed()` waits for a change. `moq_bandwidth_consumer_close()` releases demand; the last consumer stops PROBE. Stats snapshots do not request it.
- **Demand.** A watcher from `moq_publish_track_demand()` (or the media, video, and audio variants) calls back `MOQ_DEMAND_USED` or `MOQ_DEMAND_UNUSED`, again whenever the wanted group moves. `moq_publish_demand_wanted()` reads that group: start a new one once it passes your largest. Set `refresh` in `moq_track_info` to have a joining subscriber ask for a new group once the newest has grown past `refresh` times its first frame.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.

## Reference

- API source: [rs/moq-c/src](https://github.com/moq-dev/moq/tree/main/rs/moq-c/src)
- Source and a worked example: [`rs/moq-c`](https://github.com/moq-dev/moq/tree/main/rs/moq-c)
