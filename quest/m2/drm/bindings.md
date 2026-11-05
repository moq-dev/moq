# [M] Protected CMAF in the bindings

## Goal

Every binding publishes and reads protected CMAF with the same reach as Rust:

- moq-ffi's container importers take license and certificate URLs per DRM system.
- moq-ffi's catalog exposes each rendition's `drm` entries.
- Every wrapper the Cross-Package Sync table lists, and its `doc/lib` page, mirrors both.

## Plan

Decided with the maintainer on 2026-11-03: license URLs reach the bindings too, not only Rust and the CLI.
Mirror the Rust config shape and names [the import](/quest/m2/drm/import.md) lands.

## Required

- [Protected CMAF import](/quest/m2/drm/import.md) - the Rust config and the hang `drm` section the bindings expose
