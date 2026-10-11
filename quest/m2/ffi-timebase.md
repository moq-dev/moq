# [M] Shared timebase in the bindings

## Goal

A binding app (moq-ffi, moq-c, and every language wrapper) can publish
several containers from one source on one shared offset, so separate audio and
video containers stay in sync on a clock that is already in use.

Today each `publish_container` call reserves through `catalog.reserve()`, a
fresh `catalog::Timebase` per call. Two containers fed from one PTS base (say
separate audio and video fMP4) each get their own offset once the clock is
fixed, apart by the gap between their first frames' arrivals.

Non-goals: codec-level publishes (`publish_audio`, `publish_video`, and their
`_on_track` and stream forms) take caller timestamps and have no offset.
`Timebase::place` stays Rust-only until a binding consumer needs it.

## Plan

Follow-up of [moq-dev/moq#5082](https://github.com/moq-dev/moq/pull/5082),
which added `catalog::Timebase` in Rust.

Decided (2026-10-09):

- Shape: `MoqBroadcastProducer::timebase()` returns a `MoqTimebase` handle,
  and `MoqContainerInit` gains an optional `timebase` field. A container
  publish with one reserves through it; without one it reserves fresh, as
  today. `publish_container_stream(format)` becomes a breaking change taking
  an init record carrying the same field, not a `_with_timebase` variant.
  Rejected: methods on the handle (`timebase.publish_container`), which doubles
  each container entry point in every wrapper, and a positional parameter,
  which breaks the init-record convention.
- C: `moq_container_init.timebase` is a handle id, 0 for none, minted by
  `moq_publish_timebase(broadcast)` with a matching close. moq-c has no
  container-stream entry point, so only `moq_publish_container` changes.
- Name: `Timebase`, mirroring Rust (`MoqTimebase` in FFI, `Timebase` in the
  wrappers). PR 5082 renamed the Rust `catalog::Input` to it before release.
- Wrappers: Python and Swift take a defaulted keyword argument, Go an options
  field; Kotlin and Dart add one alias each (`rs/moq-ffi/AGENTS.md`).
- Docs: inline only, a note where each `doc/lib/{py,swift,kt,go,dart,c}`
  page covers container publishing. No new guide page.
- m2: no consumer has asked, and a live feed's drift is usually just network
  jitter.

Test: two containers published through one `MoqTimebase` onto a clock already
taken land their first frames on the same shifted timeline, and two without
one each shift by their own offset. Run `just test interop --all`.
