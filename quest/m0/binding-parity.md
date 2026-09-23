# [M] Every wrapper exposes the moq-ffi surface in its own idiom

## Goal

The Python, Go, Swift, Kotlin, and Dart wrappers each reach every moq-ffi
method, spell the verbs moq-net spells, and use the idiom their language
expects, so the release moq.pro pins has no binding a consumer must drop to
the generated layer for.

## Plan

Coverage is already there: an audit of every `#[uniffi::export]` method in
`rs/moq-ffi/src` against the five wrappers found none unreachable. Python and
Go re-wrap each one; Kotlin, Dart, and Swift alias the generated types, so the
methods arrive with them. What is left is idiom, and half of it cannot be done
additively.

Landed:

- Python: `OriginDynamic`, `BroadcastDynamic`, `TrackDynamic`,
  `JsonSnapshotConsumer`, and `JsonStreamConsumer` are async context managers
  like the other consumers. Public annotations name the unprefixed aliases.
- Kotlin: `VideoConsumer.frames()` matches the audio `Flow`. The public
  signatures spell the `Aliases.kt` names. `Durations.kt` reads the
  microsecond fields back as `kotlin.time.Duration`.
- Dart: `reconnect` and `backoff` reach `Moq.connect`, a `Server.listen`
  facade mirrors Kotlin's, the types have unprefixed aliases, and the
  microsecond fields read back as a `Duration`.

`fetch_media_group` and `EncodeVideo` had already moved to an options struct
before this quest ran; the plan's arity list was stale.

Open, and a maintainer call, because every one of these renames a symbol in a
released package with no additive path (`moq-rs` 0.4.7 on PyPI, `moq.dev/moq`
v0.6 on the Go mirror, `dev.moq:moq` 0.4.5 on Maven, `Moq` 0.4.6 on the Swift
mirror; only Dart is unpublished). `CLAUDE.md` forbids an alias or a
`foo_with_x`, so each one is a break to schedule on `dev` or to decide against:

- `subscribe` to `consume` on Python `Client`/`connect`, Kotlin
  `Moq.connect`/`Server.listen`, Dart `Moq.connect`, and Go
  `WithSubscribeOrigin`/`WithServerSubscribeOrigin`. Swift already spells it
  `setConsume`.
- Go's `All`/`Requests`/`Updates`/`Frames`/`Values` to one verb. These are the
  `iter.Seq2` helpers, not `Next`, which every consumer already has, so the
  rename is about the range-over-func name alone.
- Kotlin `announcements` and Dart `announcements` to `announced`. Both already
  have an `announced` returning the raw handle, so this is a collision rather
  than a gap: the two names are the `Flow`/`Stream` and the cursor.
- Kotlin `Moq.connect`'s twelve named parameters to an options struct, which
  `rs/moq-ffi/CLAUDE.md` asks for.
- Go `EncodeAudio` (4 args past `ctx`) and `FetchMediaGroup` (5) to an options
  struct. Go has no overloads, so there is no additive spelling.
- `rtt_us` and friends as a `timedelta` in Python and a `time.Duration` in Go.
  Python's records are the generated dataclasses and Go's `ConnectionStats` is
  a type alias, so neither can gain an accessor without redefining the type.
  Kotlin and Dart have extensions and are done.

Public API: additive on the wrappers. Wire: none. Binding parity gates
the release, not the merge.

## Related

- [Release](/quest/m0/release.md) - names this as a release gate
- [Binding docs](/quest/m1/binding-docs.md) - the pages that describe each wrapper
