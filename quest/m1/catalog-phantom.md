# [M] Producers drop the phantom catalog type

## Goal

Media and data producers that only reference the catalog through an erased
handle stop carrying its extension type. moq-video's and moq-audio's
`encode::Producer`, and moq-mux's `json::Snapshot`, `json::Stream`,
`binary::Snapshot` and `binary::Stream`, lose their `E: CatalogExt`
parameter, so callers with a custom catalog no longer thread a generic that
holds no data. Constructors stay generic over `E` where they take a typed
`Reserved<E>` or catalog `Producer<E>`.

## Plan

Found by the 2026-10-22 refactor audit (main at d44813cb7). `E` lives on
those types only as `PhantomData<fn() -> E>`: mux's `Listing` already erases
the entry type, and the encoders hold no catalog after construction.

Decided 2026-10-22 (maintainer): drop `E` only where it is phantom. It stays
on `Reserved<E>`, `Timebase<E>`, the catalog `Producer<E>` and capture
drivers that own a typed catalog, because those store it. JSON's `T` stays.
No dynamic dispatch is added.

Update every consumer in the same PR: moq-ffi (`video.rs`, `audio.rs`,
`json.rs`, `flate.rs`), moq-c, and any example or test naming the parameter.
The PR's Impact bullet says how to migrate in moq-video, moq-audio and
moq-mux. Do not claim a binary size change without measuring it.

Public API: breaking Rust (type parameter removed). Wire: none.
