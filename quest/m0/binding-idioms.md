# [M] Every wrapper spells moq-net's verbs and groups its surface into handles

## Goal

The Python, Go, Swift, and Kotlin wrappers drop the spellings and flat methods
that the additive half of binding parity superseded, so each capability has
one name, moq-net's verb, reached through one handle.

## Plan

Each item renames or removes a symbol in a released package (`moq-rs` on PyPI,
`moq.dev/moq` on the Go mirror, `dev.moq:moq` on Maven, `Moq` on the Swift
mirror), and `CLAUDE.md` forbids an alias or a `foo_with_x`, so each one is a
break. Dart is unpublished and already took its share on main.

Handles, whose additive half is on main:

- Remove `set_video_properties`, `set_catalog_section`, and
  `remove_catalog_section` from `MoqBroadcastProducer` and every wrapper;
  `catalog()` returns the `CatalogProducer` that owns them.
- Remove the flat `used`/`unused` from the track, media, audio, and video
  producers in favor of `demand()` (#3949).
- Kotlin: remove `OriginConsumer.announcements` and `Moq.announcements`;
  `announced(config).updates()` is the `Flow`.

Renames:

- `subscribe` to `consume` on Python `Client`/`connect`, Kotlin
  `Moq.connect`/`Server.listen`, and Go
  `WithSubscribeOrigin`/`WithServerSubscribeOrigin`. Swift spells it
  `setConsume` and Dart `ConnectOptions.consume`.
- Go's `All`/`Requests`/`Updates`/`Frames`/`Values` to one verb. These are
  the `iter.Seq2` helpers, not `Next`, so the rename is the range-over-func
  name alone.

Options structs, which `rs/moq-ffi/CLAUDE.md` asks for:

- Kotlin `Moq.connect`'s twelve named parameters, like Dart's
  `ConnectOptions`.
- Go `EncodeAudio` (4 args past `ctx`) and `FetchMediaGroup` (5). Go has no
  overloads, so there is no additive spelling.

Durations: `rtt_us` and friends as a `timedelta` in Python and a
`time.Duration` in Go. Python's records are the generated dataclasses and Go's
`ConnectionStats` is a type alias, so neither can gain an accessor without
redefining the type. Kotlin and Dart have extensions and are done.

Open: `Moq` (Kotlin, Dart) and `Client` (Python, Go) re-expose
`createBroadcast`, `announced`, `announcedBroadcast`, and `requestBroadcast`
from `session.publish()`/`session.consume()`. Keeping them flat is the same
spread the handles above remove; dropping them costs every quick-start one
more hop. Decide before cutting the release.

Public API: breaking on the four published wrappers and moq-ffi. Wire: none.

## Related

- [Binding docs](/quest/m1/binding-docs.md) - the pages that describe each wrapper
