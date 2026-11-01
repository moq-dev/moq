# [XS] moq-quic's internal dev-dependencies are path-only

## Goal

moq-quic's dev-dependency on moq-congestion uses a path, as every internal dev-dependency should, so publishing moq-quic never needs a matching moq-congestion release. External dev-dependencies take the workspace version, so moq-tokio no longer pins its own `tempfile`.

## Plan

Found by the agent on [moq-dev/moq#7377](https://github.com/moq-dev/moq/pull/7377): moq-quic's `[dev-dependencies]` take moq-congestion with `workspace = true`. Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Switch it to a path and check no other crate has the same pattern (`cargo metadata`, or grep the `[dev-dependencies]` tables).

Widened 2026-10-31 from [moq-dev/moq#7260](https://github.com/moq-dev/moq/pull/7260): `rs/moq-tokio/Cargo.toml` pins its dev-dependency `tempfile = "3"` directly, while every other crate takes `tempfile = { workspace = true }`. Switch it to the workspace entry in the same PR, and check the dev-dependency tables for other external crates pinned outside the workspace.

Public API: none. Wire: none.
