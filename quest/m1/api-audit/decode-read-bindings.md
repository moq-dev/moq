# [L] Bindings read decoder gaps and ends

## Goal

Every binding reads the same audio and video decode stream as Rust:
frames, a gap where groups were skipped, and the end. moq-ffi, the generated
C and C++, Python, Swift, Kotlin, Dart, and Go stop discarding `Gap` and
`End`.

## Plan

moq-audio and moq-video yield `decode::Read { Frame, Gap, End }`, but moq-ffi (`audio.rs`,
`video.rs`), the hand-written moq-c, and the interop audio client still
hand over frames only.

Open for this quest: the binding shape. Options are a tagged record per read
(mirrors Rust, one call), or frames plus a separate gap and end signal
(smaller change for wrappers). Recommend the tagged record, mirroring Rust
names, and ask the maintainer before implementing.

Breaking in moq-ffi and every wrapper: `MIGRATE.md` entries per package, and
the Cross-Package Sync table's docs (`doc/lib/*`). Run `just test interop --all`.

Decided 2026-10-23 (maintainer): tracked under the API audit.

Public API: breaking, every binding. Wire: none.
