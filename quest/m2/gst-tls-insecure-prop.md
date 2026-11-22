# [XS] Rename moq-gst tls-disable-verify property to connect-tls-insecure

## Goal

`moq-gst`'s GObject property matches the renamed `connect-tls-insecure` naming
used across the rest of the workspace instead of shipping the retired
`tls-disable-verify` name in `0.22.0`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ rename `tls-disable-verify` to `connect-tls-insecure`
in `m2` before `0.22.0` freezes the GStreamer element properties (rejected:
skipping the API freeze cleanups).

- Update `rs/moq-gst/src/connection.rs:11,22,50-54,80,92,115` to rename the
  `tls-disable-verify` GObject property and field/accessor references to
  `connect-tls-insecure`.
- Grep the repository (`rs/moq-gst`, `doc/`, `test/`, `MIGRATE.md`) for
  `tls-disable-verify` and update every pipeline string, test, and doc
  reference.

Public API: breaking in `moq-gst` (GObject property rename; add a `MIGRATE.md`
note during migration audit or in the PR). Wire: none. Run `moq-gst` unit and
pipeline tests.
