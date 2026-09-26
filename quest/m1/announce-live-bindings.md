# [M] Bindings see when an announce consumer has caught up

## Goal

moq-ffi, libmoq, and every wrapper (`py`, `swift`, `kt`, `go`, `dart`) yield
the same flat announce event Rust does: `Announced`, `Updated`, or `Retracted`
carrying the announce, or `Live` once every route live at subscribe time has
been delivered. A binding app can list what is live and stop, with no timer.

## Plan

Mirror moq-net's `announce::Event` one to one:

- moq-ffi: `MoqAnnounceConsumer::next` returns `MoqAnnounceEvent`, a uniffi
  enum with the four variants, replacing `MoqAnnounceUpdate::active()`. Stop
  filtering `Live` in `rs/moq-ffi/src/origin.rs`.
- libmoq: the `on_announce` handle's `moq_announce_update.kind` gains a LIVE
  value with no route fields, replacing the active flag. Stop skipping
  `Event::Live` in `rs/libmoq/src/origin.rs`.
- Hand-written wrappers and `doc/lib/{py,swift,kt,go,dart,c}` follow, with a
  test per binding that an empty origin still yields `Live`.

Public API: breaking in moq-ffi, libmoq's C ABI, and every wrapper, so it
retargets to `dev` with the Rust break. Wire: none.
