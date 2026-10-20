# [M] Document the moq-mux public API

## Goal

Every public item in `moq-mux` has a one-line rustdoc, and the crate sets
`#![warn(missing_docs)]` so new items stay documented.

## Plan

The m0 documentation audit (2026-10-19) found 205 undocumented public items
with default features, too many to review alongside its other fixes. They
cluster in `container/fmp4` (43), `catalog/msf` (28), `container/mkv` (36),
and the `codec` modules (h264 27, h265 19, av1, annexb, aac, vp8, vp9).

Count them with
`cargo rustdoc -p moq-mux --lib --all-features -- -W missing_docs` and keep
it clean. `just check` does not pass `--all-features`; the nightly `just rs
features` recipe does, so run that rustdoc command before landing. Struct
fields that mirror a spec (ISO BMFF boxes, Matroska elements, SPS fields) can
name the spec field instead of restating it. An item that exists only for
tests or another crate's internals is a candidate for `pub(crate)` instead of
a doc; narrowing one is a breaking change, so list it in `MIGRATE.md`.
