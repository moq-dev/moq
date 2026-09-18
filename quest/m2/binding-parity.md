# [M] Every wrapper exposes the moq-ffi surface in its own idiom

## Goal

The Python, Go, Swift, Kotlin, and Dart wrappers each reach every moq-ffi
method, spell the verbs moq-net spells, and use the idiom their language
expects, so the release moq.pro pins has no binding a consumer must drop to
the generated layer for.

## Plan

Gaps found method by method against `rs/moq-ffi/src`:

- Dart: `set_reconnect`/`set_backoff` are not wired into `Moq.connect`,
  there is no `Server` facade, and every type keeps its `Moq` prefix.
- Kotlin: no `Flow` for `MoqVideoConsumer` where audio has one; `Moq*`
  types leak through the public API despite `Aliases.kt`; `connect` takes
  twelve named parameters where `rs/moq-ffi/CLAUDE.md` says Go, Kotlin, and
  Dart take an options struct (only Go does).
- Python: `OriginDynamic`, `TrackDynamic`, `BroadcastDynamic`,
  `JsonSnapshotConsumer`, and `JsonStreamConsumer` own a `cancel()` with no
  context manager; `fetch_media_group` takes four positional arguments;
  annotations name `MoqAudioFormat` where unprefixed aliases exist.
- Go: `EncodeAudio`, `EncodeVideo`, `PublishAudioOnTrack`, `FetchGroup`,
  `FetchMediaGroup`, `SubscribeMedia`, `DecodeAudio`, and `DecodeVideo` take
  four or more positional arguments.
- Verbs: `announced` is `announcements` in Kotlin and Dart; `next` is
  `All`/`Requests`/`Updates`/`Frames`/`Values` in Go; `set_consume` is
  `subscribe=` in four. The wrappers move to the core's spelling; the two
  core changes (`set_tls_verify`, the transport enum) land on dev in
  [libmoq units](/quest/m1/api-libmoq-units.md).
- `rtt_us` and the other microsecond fields are raw integers in every
  wrapper; a `Duration`/`timedelta` at the boundary where the language has
  one.

Public API: additive on the wrappers. Wire: none. Binding parity gates
the release, not the merge.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the surface is final after it

## Related

- [Release](/quest/m1/release.md) - names this as a release gate
- [Binding docs](/quest/m2/binding-docs.md) - the pages that describe each wrapper
- [Binding audio tests](/quest/m2/binding-audio-tests.md) - the proof per binding
