# [M] Bindings expose epochs

## Goal

`moq-ffi` and the py, swift, kt, go, and dart wrappers can announce an epoch like Rust, with a minting helper. The generated C and C++
bindings pick it up from moq-ffi; libmoq gets no new API. The epoch on an announced route is
readable, and a publisher can pass an explicit one. Every wrapper surfaces
the `Restart` announce event. The reconnect counter `session.epoch()` and the
audio timeline re-anchor `reset_epoch` are renamed so "epoch" has one meaning.

## Plan

- Expose the parsed epoch (text and time) and an explicit-epoch publish
  argument. Keep the surface to what a binding consumer needs.
- Rename the reconnect counter, `moq_tokio::Connection::epoch()` and
  `session.epoch()` in every binding (for example to `connects()`). That is
  a break.
- Rename the audio re-anchor the same way (decided 2026-10-08, since it also
  says "epoch"): `moq_audio::encode::Producer::reset_epoch`, its moq-ffi
  method, the wrappers that expose it, and moq-boy's copy.
- `MoqAnnounceEvent::Restart` lands in moq-ffi with
  [Restart](/quest/m0/broadcast-epoch/restart.md); each wrapper maps it.
- Update `doc/lib/{py,swift,kt,go,dart}` per the cross-package sync table,
  and run `just test smoke --all`.

Decided in the 2026-09-30 audit: libmoq is frozen (renamed `rs/moq-c`), so C and C++ consumers get epochs from moq-ffi.

Decided in the 2026-10-05 audit: this lands before the
[FFI shape](/quest/m1/ffi-shape/README.md) line (#4519), which reshapes the
same wrappers and keeps `epoch()` today. It rebases onto this quest and
adopts the epoch surface and the rename, so the wrappers break once each.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - adds the `Restart` announce event the wrappers expose
