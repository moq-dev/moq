---
title: C
description: moq-c, the stable C ABI over the Rust core
---

# C

[![GitHub release](https://img.shields.io/github/v/release/moq-dev/moq?filter=moq-c-v*\&label=moq-c)](https://github.com/moq-dev/moq/releases?q=moq-c)

`moq-c` exposes MoQ to C, C++, and any language with a C FFI through a
stable ABI: a generated `moq.h`, a static `libmoq.a` that links the whole Rust
runtime in, and a pkg-config file for its native link dependencies. The
[OBS plugin](/bin/obs) is built on it. Releases through 0.6 were named
`libmoq`.

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
`cargo build -p moq-c` leaves `moq.h` in a hashed `OUT_DIR` and writes no
pkg-config file.

## Connect

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

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) is
here, from media publish and consume to raw pixels and PCM with the codec
inside. The header is the reference: each function carries a doc comment.

- **Handles and callbacks.** Every object is an integer handle, and every async result arrives on a `moq_status_callback` with your `void *user_data`. A status `> 0` is a live result, `0` a clean close, and `< 0` an error. The last two are terminal: moq-c never touches `user_data` again, so free it there. `*_close` and `*_cancel` only request shutdown; the terminal callback still fires.
- **Errors.** Negative return codes are named by `MOQ_ERROR_*`, and `moq_error()` gives the reason on the calling thread. Auth rejections have their own codes so you don't retry them. For the peer's protocol code use `moq_error_protocol()`; do not parse `moq_error()`.
- **Threading.** Call any function from any thread. Raw publish calls block until the codec takes the frame, which paces a publisher.
- **Zeroed config is the default.** A zeroed `moq_client_config` means the default for every field, so new fields don't break callers. A field whose default isn't zero has a `has_*` flag: `backoff_timeout_us = 0` means "retry forever" only with `has_backoff_timeout = true`.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `moq_publish_media_cut`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `moq_publish_media_flush` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `moq_publish_media_discontinuity`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** Free each frame id with `moq_decode_video_frame_free` promptly, or the decoder stalls.
- **Stats.** `moq_session_stats()` fills `moq_connection_stats` with `rtt_us`, `estimated_send_rate_bps`, `estimated_recv_rate_bps`, and the byte and packet counters (`bytes_sent`, `bytes_received`, `bytes_lost`, `packets_sent`, `packets_received`, `packets_lost`). Each has a `<field>_valid` flag, false when the transport does not report it, which is not the same as zero.

## Reference

- API reference: [docs.rs/moq-c](https://docs.rs/moq-c)
- Source and a worked example: [`rs/moq-c`](https://github.com/moq-dev/moq/tree/main/rs/moq-c)
