# [XS] moq-native drops its empty noq feature

## Goal

`rs/moq-native` no longer declares the empty `noq = []` feature it enables
by default, and every reference to it (other crates' feature lists, docs,
CI matrices) is gone.

## Plan

Found 2026-10-20 auditing dependencies on the web-transport repo, noq and
quinn after the maintainer decided moq's own QUIC stack, `moq-quic`, powers
everything. The feature gates nothing.

- Removing a feature is breaking for anyone who names it: add an entry
  under `## Unreleased` in moq-native's `MIGRATE.md`.

Public API: the `noq` Cargo feature is removed. Wire: none.
