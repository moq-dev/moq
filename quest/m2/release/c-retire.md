# [S] Retire the hand-written moq-c crate

## Goal

The hand-written C crate is gone: its source, workflows, and docs that only
served it are deleted once the generated `moq-c` publishes. The generated
`moq-c` package's `MIGRATE.md` tells C users how to move.

## Plan

- #4288 renamed libmoq to `moq-c` (`rs/moq-c`); this quest deletes that
  hand-written crate.
- Decided 2026-10-20 (maintainer): no final hand-written notice release,
  because the hand-written `moq-c` never published to a registry; the
  generated package is the first `moq-c` users install.
- `rs/moq-c/native-libs/` serves the generated package too, and
  `cpp/moq/cmake/moq-ffi.cmake` serves both C++ and C. Move them to a neutral
  home shared by both packages before deleting the crate.
- The code-free `rs/libmoq` stub left under the old name is
  [Retire the libmoq stub](/quest/m2/release/libmoq-retire.md)'s, separately.

## Required

- [Generated C publication](/quest/m2/release/c-publish.md) - the replacement is available and verified

- [C consumers](/quest/m1/c/consumers.md) - nothing in the repository still calls the hand-written ABI

## Related

- [Retire the libmoq stub](/quest/m2/release/libmoq-retire.md) - deletes the `rs/libmoq` stub under the old name
