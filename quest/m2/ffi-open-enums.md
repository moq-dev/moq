# [M] Open closed extensible moq-ffi enums and derive codec traits

## Goal

Every extensible enum exported by `moq-ffi` follows `rs/moq-ffi/AGENTS.md`'s
extensibility rule (no closed `uniffi::Enum` where adding a format/container
breaks bindings), and `MoqAudioCodec` / `MoqVideoCodec` implement `Debug`,
`Eq`, `Hash`, and a `name()` accessor.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix all 5 closed extensible `uniffi::Enum`s and derive
`Debug`/`Eq`/`Hash` on `MoqAudioCodec`/`MoqVideoCodec` in `m2` before `0.22.0`
publishes the bindings (rejected: deferring the enum freeze to `m3` / `0.23`;
skipping the API freeze cleanups).

1. **Open the 5 closed extensible `uniffi::Enum`s** per `rs/moq-ffi/AGENTS.md`
   (switch to open UniFFI objects / string-backed constants, or an explicit
   `Unknown` fallback variant for returned enums; match the repo's existing
   pattern for `MoqAudioCodec` and `MoqVideoCodec`):
   - `MoqContainer` (`rs/moq-ffi/src/media.rs:58`, returned enum)
   - `MoqAudioFormat` (`rs/moq-ffi/src/media.rs:318`)
   - `MoqVideoFormat` (`rs/moq-ffi/src/media.rs:335`)
   - `MoqContainerFormat` (`rs/moq-ffi/src/media.rs:354`)
   - `MoqVideoPixelFormat` (`rs/moq-ffi/src/video.rs:24`)
2. **Add `Debug`, `Eq`, `Hash`, and `name()` to codecs:**
   - Derive/export `Debug`, `Eq`, `Hash` (and a `name(&self) -> String` helper
     if missing) on `MoqAudioCodec` (`rs/moq-ffi/src/audio.rs:62`) and
     `MoqVideoCodec` (`rs/moq-ffi/src/video.rs:40`) so Kotlin, Swift, Python,
     and Dart callers can log, compare, and key maps by codec.
3. **Update all binding wrappers and tests** (`kt/`, `swift/`, `py/`, `dart/`,
   `cpp/`, `rs/libmoq` if mirrored) to use the updated open types.

Public API: breaking in `moq-ffi` and its language bindings (Kotlin, Swift,
Python, Dart, C++). Wire: none. Run FFI Rust tests and binding build/test
suites.
