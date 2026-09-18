# [S] libmoq's config speaks one unit and its header declares every enum

## Goal

A C caller of `moq.h` reads every duration in microseconds, finds every
enum the structs use, and gets a terminal callback for every task it
registers. The 0.5 header is already rewritten on dev (41 client setters
became `moq_client_config`), so this is the release to finish it.

## Plan

- `moq_client_config` mixes `connect_timeout_ms`, `failover_delay_ms`,
  `resolution_delay_ms`, `websocket_delay_ms`, `quic_idle_timeout_ms`, and
  `quic_keep_alive_ms` with `backoff_*_us` in one struct; every field is
  `_us`, matching the microseconds rule #3744 set for the bindings. OBS
  (`cpp/obs/src/moq-settings.cpp`) converts at its dials.
- `rs/libmoq/build.rs` lists `moq_audio_format` among the enums cbindgen
  must emit, but the PCM layout enum was renamed `moq_audio_sample_format`
  and `moq_audio_format` is now the codec enum; the sample format reaches
  no signature (its fields are `u32`), so the generated header omits it and
  a C caller hardcodes 0 to 7. Add it to the list and delete the stale
  "named `_max`" comment on `moq_audio_decoder_output.max_age_us`.
- Every registrar refuses a NULL callback with `InvalidPointer`, as
  `moq_origin_dynamic` already does; the other twelve accept `None`, never
  fire the terminal, and leak the `user_data`.
- `moq_origin_dynamic_close` returns a negative code on a second call as
  its doc says; the implementation is the one `_close` without `ok_or`.
- `#define MOQ_ERROR_*` for the 38 codes in `error.rs` (the header has none)
  and a comment reserving the retired -1, -11, -12, -39.
- One `typedef` for the status callback in place of fourteen inline
  signatures.
- Every task's `_close` becomes `_cancel`, the verb the uniffi core and
  every wrapper use, and `moq_origin_consume_announced` becomes
  `moq_origin_announced_broadcast` to match `announced_broadcast`; fourteen
  functions plus `cpp/obs`. One break now instead of a second later.
- In moq-ffi, on the same release: `set_tls_disable_verify(bool)` becomes
  `set_tls_verify(bool)`, the polarity every wrapper already inverts by
  hand, and `MoqRequest::transport` is an enum instead of a `String` that
  Go turns into an open-set type and Python into a `Literal`. Both ripple
  through the five wrappers and `doc/lib/*` per the Cross-Package Sync
  checklist.
- `CHANGELOG.md` `[Unreleased]` names the setter deletion, the bandwidth
  functions, `create_broadcast`/`announce`/`dynamic`, the decoder output
  format, `moq_route`, the `estimated_*_rate` stats fields, and
  `frame_duration_us`; the `[0.5.14]` section holds unreleased work under
  a released heading. `doc/lib/c/index.md` still advertises
  `moq_publish_video_raw` and the `_consume_*_raw` mirrors.

Public API: breaking on libmoq's C ABI, so on dev. Wire: none. Consumers:
`cpp/obs`, `doc/bin/obs.md`, `doc/lib/c`.

## Related

- [Binding parity](/quest/m2/binding-parity.md) - the uniffi wrappers' half
- [Track demand](/quest/m1/libmoq-track-demand.md) - lands on the same header
