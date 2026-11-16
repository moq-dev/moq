# [M] Binding selectors can grow, and a null max age means unlimited

## Goal

A value in the bindings that names an open set, one Rust can add a member
to, never forces a breaking binding release when it grows.
`rs/moq-ffi/AGENTS.md` states the rule: a selector a caller passes is an
object with constructors, and an enum a caller reads has a catch-all. `MoqVideoCodec` and
`MoqCompression` are objects, as `MoqAudioCodec` and `MoqCongestionControl`
already are; `MoqTransport` and `MoqVideoSurface` have an unknown variant in
moq-ffi, so the wrappers' hand-written fences are gone.

In `MoqTrackInfo`, a null `max_age` means no limit, as a null `MoqDelay.max`
does and as Rust, JS, and Python already read it, and `max_age_unlimited` is
gone.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Open the selectors: one rule in `rs/moq-ffi/AGENTS.md`, applied to
  `MoqVideoCodec`, `MoqTransport`, `MoqVideoSurface`, and `MoqCompression`
  (rejected: leaving them and treating every new variant as a break; also
  collapsing `MoqError`, which is worth it only if the bindings were not
  about to change exporter).
- ✅ `TrackInfo.max_age` null means unlimited and `max_age_unlimited` is
  deleted (rejected: keeping both fields; leaving it for the exporter).
- Not selected, so unchanged: the `Moq` facade Kotlin and Dart start from,
  where the other languages start from `Client`.
- The maintainer accepted that the bindings break again when
  [the exporter switch](/quest/m3/ffi-exporter.md) lands, so this quest
  makes only the reshapes that outlive the generator: a rule and these six
  types.

Facts found by the audit:

- Swift, Kotlin, and Dart switch over enums exhaustively and the wrappers
  alias the generated enums, so a new variant breaks source. Two selectors
  were made objects for this reason: `MoqAudioCodec`
  (`rs/moq-ffi/src/audio.rs:58-62`, "so adding a codec later does not break
  callers switching over a closed enum") and `MoqCongestionControl`
  (`session.rs:148`).
- Their siblings are closed while the Rust they mirror is open:
  `MoqVideoCodec` (`video.rs:40`, two variants; AV1 or VP9 is a break);
  `MoqTransport` (`server.rs:429`), where an unmapped transport makes
  `transport()` fail with `Unsupported` (`:452`); `MoqVideoSurface`
  (`video.rs:1126`), one variant with Android's `HardwareBuffer` planned,
  which each wrapper fences by hand (`swift/Sources/Moq/Video.swift:62-75`,
  and Swift's fence is itself a closed enum); `MoqCompression`
  (`producer.rs:69`), one variant.
- `MoqTrackInfo.max_age` is "null for the default, zero", with
  `max_age_unlimited` overriding it (`producer.rs:42-51`, conversion at
  `:88-92`), while reading inverts it: `info()` reports unlimited as
  `max_age: None` plus the flag (`:120`). `MoqDelay.max` is null for no
  limit. Swift, Kotlin, Dart, C++, and generated C see the raw record, so
  `TrackInfo(maxAge: nil)` serves only the live edge.

For the PR:

- Write the rule first, in `rs/moq-ffi/AGENTS.md`: a selector naming an open
  set is an object; an enum a caller reads carries a catch-all in moq-ffi,
  not in each wrapper; a null duration means no limit. Recommended third
  clause: a new variant of a closed enum waits for an API cut, since
  `sh/api-diff/run.sh` counts a line added inside an existing declaration as
  a break (`:54-79`).
- `MoqVideoCodec` and `MoqCompression` become objects with one constructor
  per member and a name accessor, following `MoqAudioCodec`. `MoqCompression`
  is also read back from `info()`, so it needs equality or a name a caller
  can compare.
- `MoqTransport` and `MoqVideoSurface` gain the catch-all; `transport()`
  stops failing on a transport it cannot name; delete each wrapper's fence
  (Swift, Dart, Python, Kotlin, Go) and take the generated one.
- `max_age` becomes a required field with no default, null for unlimited,
  as `MoqSubscription.delay` is required. Python, Go, and Dart already
  default an omitted value in the wrapper; check each still defaults to
  zero, and that Swift and Kotlin callers must now state it. Remove the
  flag from moq-ffi, the hand-written C API (`rs/moq-c/src/api.rs`), every
  wrapper, and their tests.
- Follow the sync table from `rs/moq-ffi` outward: every wrapper, `cpp/moq`,
  `cpp/obs` if it names a codec, each `doc/lib` page, and the Impact section
  for each package's `MIGRATE.md`. Run `just test interop --all`.
- Fix `swift/README.md:67` while in the Swift docs: it says each `MoqError`
  case carries `message: String`, which `swift/Sources/Moq/Errors.swift`
  does not have.
- [Track info docs](/quest/m4/track-info-docs.md) describes the record after
  this lands.

Public API: breaking in every binding (two enums become objects, two gain a
variant, `max_age` changes meaning and `max_age_unlimited` is removed).
Wire: none.

## Related

- [Switch moq's bindings to rust2's exporter](/quest/m3/ffi-exporter.md) - the second binding break, which must keep these shapes
- [Bindings address groups by start](/quest/m2/time-addressing/bindings.md) - the other binding rename before the cut
