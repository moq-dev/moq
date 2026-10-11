# [M] Shared timebase in the bindings

## Goal

A binding app can publish several containers from one source on one shared
offset, so separate audio and video containers stay synchronized on a clock
already in use. Container constructors currently reserve a fresh
`catalog::Timebase` per call, making their offsets depend on first arrival.

## Plan

Decided in the 2026-10-10 audit, adapting the approved shared-offset behavior
to the media constructors introduced by #4519:

- `Media.Timebase(broadcast)` constructs the shared handle, mirrored as
  `MoqMediaTimebase` in FFI and `Timebase` under each wrapper's media namespace.
  Prefer this constructor over a factory on CatalogProducer; it needs no
  extra catalog handle and keeps media methods off the net broadcast API.
- Container init carries an optional timebase. Present uses that shared
  reservation; absent preserves a fresh offset. The container stream
  constructor takes an init record with format and optional timebase instead
  of a bare format. Change the constructor, not a compatibility variant.
- A timebase belongs to one broadcast/catalog. Refuse pairing it with another;
  do not let the handle independently keep a publication alive. Mirror the
  current media handle ownership rather than introducing a second lifetime.
- Apply it to current media container constructors and every wrapper. C uses
  the generated binding surface; the hand-written C API gets no new features.
- Codec producers still take caller timestamps with no offset. `Timebase::place`
  stays Rust-only until a consumer needs it. Retain m2 priority.
- Update existing container-publishing docs inline. No additional timebase guide.

Test two containers sharing an offset after the clock is already taken,
independent offsets when omitted, foreign-broadcast refusal, and handle
teardown. Use controlled time and existing interop CI; run
`just test interop --all` and `just check`.

Public API: media Timebase and optional init fields across bindings; the
stream constructor takes a record. Wire: none.

## Related

- [Generated C](/quest/m1/c/README.md) - mirrors the FFI surface instead of extending the retired hand-written API
