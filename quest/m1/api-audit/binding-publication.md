# [L] Binding publications

## Goal

In moq-ffi, the C ABI, and every wrapper, a broadcast producer fills tracks
and cannot announce. Creating a broadcast on an origin and publishing a
prepared answer return a publication handle that carries announce and
unannounce, mirroring Rust's `origin::Publication`, so a standalone or
prepared broadcast no longer exposes announcement calls that can only fail.

## Plan

Decided 2026-10-23 (maintainer, on
[moq-dev/moq#6464](https://github.com/moq-dev/moq/pull/6464)): ✅ land the
announcement split in three PRs: native Rust in that PR, the
[JS mirror](https://github.com/moq-dev/moq/pull/6795), and this binding rollout
once the native PR merges. The native PR keeps private adapters that hold the
`Publication` beside each binding fill handle (`rs/moq-ffi/src/producer.rs`
`from_publication`, `rs/moq-c/src/publish.rs` `create_publication` and
`admit`); this quest replaces them with public handles.

The shape is Rust's: an origin-owned record holding the fill broadcast and its
announcement capability, with the existing fill lifetime (dropping the
publication does not retract on its own).

- moq-ffi: a `MoqPublication` object with `broadcast()`, `announce`
  (taking `MoqAnnounceRoute`), and `unannounce`.
  `MoqOriginProducer::create_broadcast` and `MoqBroadcastPrepared::publish`
  return it; `MoqBroadcastProducer` loses `announce` and `unannounce`.
- C ABI, recommended shape: `moq_origin_create_broadcast(origin, path,
  path_len, *broadcast_out)` returns a publication handle and writes the fill
  handle to `broadcast_out`, as `moq_broadcast_request_prepare` already does.
  `moq_broadcast_prepared_publish` returns a publication handle.
  `moq_publish_announce` and `moq_publish_unannounce` become publication calls
  taking that handle, with a call to release it. Admission keeps checking that
  the prepared fill is the broadcast it prepared.
- Wrappers: py, swift, kt, dart, `go/wrapper`, `cpp/moq`, and `cpp/obs`, with
  their tests, READMEs, `doc/lib/{py,swift,kt,go,dart,c,cpp}`, `doc/bin/obs.md`,
  and a `MIGRATE.md` entry in each package. Regenerate bindings from source.

Test in each layer: clone/drop, explicit unannounce, prepared
admission (including a mismatched fill), and a stale publication against a
replacement. Run `just test interop --all`.

Public API: breaking in moq-ffi, the C ABI, and every wrapper. Wire: none.
