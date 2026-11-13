# [XS] Dev-dependencies take the workspace version

## Goal

External dev-dependencies take the workspace version, so moq-tokio no longer pins its own `tempfile`, and internal dev-dependencies use a path.

## Plan

Widened 2026-10-31 from [moq-dev/moq#7260](https://github.com/moq-dev/moq/pull/7260): `rs/moq-tokio/Cargo.toml` pins its dev-dependency `tempfile = "3"` directly, while every other crate takes `tempfile = { workspace = true }`. Switch it to the workspace entry, and check the dev-dependency tables for other external crates pinned outside the workspace, and for internal crates taken with `workspace = true` (`cargo metadata`, or grep the `[dev-dependencies]` tables).

The original case, moq-quic's dev-dependency on moq-congestion (found on [moq-dev/moq#7377](https://github.com/moq-dev/moq/pull/7377)), is gone: moq-congestion is now a normal dependency of moq-quic (`rs/moq-quic/Cargo.toml:39` at 4bf501b535), where `workspace = true` is right.

Moved to m5 2026-11-12 (maintainer, /quest-audit): no consumer; parked rather than deleted.

Public API: none. Wire: none.
