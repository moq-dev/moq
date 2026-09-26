# [XS] Retire the libmoq stub

## Goal

`rs/libmoq` is gone and the `libmoq` crate is no longer published. Its last
release, a code-free crate whose README points at `moq-c`, stays on crates.io
for anyone still resolving the old name.

## Plan

- Delete `rs/libmoq` and its workspace member once that release is on
  crates.io, so release-plz never publishes it again.
- Grep for any remaining `libmoq` that names the crate rather than the
  `libmoq.a` file.

## Required

- The final `libmoq` release, built from the `rs/libmoq` stub, is on crates.io
