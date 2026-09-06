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

- **Handles and callbacks.** Every object is an integer handle; every async result arrives on a callback with a `void *user_data`. A status `> 0` is a live result, `0` a clean close, `< 0` an error, and the last two are terminal: libmoq never touches `user_data` again, so free it there. `*_close` only requests shutdown; the terminal callback still fires.
- **Errors.** Negative return codes, with `moq_error()` giving the reason for the last failure on the calling thread. Auth rejections (401, 403) have their own codes so you don't retry them.
- **Threading.** Any function from any thread. Raw publish calls block until the codec takes the frame, which paces a publisher.
- **Client config.** A zeroed `moq_client_config` means the defaults for every knob, which is what lets a new one be appended without disturbing callers. Fields cover protocol (`versions`), TLS (`tls_fingerprints`, `tls_roots`, `tls_cert`/`_key`, `tls_host_name`), transport (`backend`, `bind`, `connect_timeout_ms`, the Happy Eyeballs delays, `websocket_enabled`), and tuning (reconnect backoff, `quic_*`). A knob whose default isn't zero carries a `has_*` flag, so setting `backoff_timeout_ms = 0` needs `has_backoff_timeout = true` to mean "retry forever" rather than "use the default". `moq_client_defaults()` reports what a NULL config dials with.
- **Everything the bindings can do** ([list](/lib/#what-every-binding-can-do)): media publish and consume with the catalog managed for you, raw pixels and PCM with the codec inside (`moq_publish_video_raw`, `moq_publish_audio_raw`, and the `_consume_*_raw` mirrors), raw tracks with timestamps and datagrams, JSON snapshot and stream tracks, group fetch, dynamic tracks and broadcasts, catalog sections, shared video properties, and stalled hints.

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

The header is the reference; each function carries a doc comment. Source and
a worked example: [`rs/libmoq`](https://github.com/moq-dev/moq/tree/main/rs/libmoq),
API docs on [docs.rs/libmoq](https://docs.rs/libmoq).
