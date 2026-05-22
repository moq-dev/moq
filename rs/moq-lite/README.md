# moq-lite (deprecated)

> **This crate has been renamed to [`moq-net`](https://crates.io/crates/moq-net).**

The old name caused confusion because `moq-lite` is also the name of one of the wire
protocols this library speaks. The crate has been renamed to `moq-net` to make clear
that it is the **networking layer** for Media over QUIC. Under the hood it negotiates
either the `moq-lite` protocol or the full IETF `moq-transport` protocol at session setup.

## Status

`moq-lite` now re-exports `moq-net` so existing code keeps building without changes.
**It will not receive further updates** — new features and breaking changes ship on
`moq-net` only. Migrate at your convenience.

## Migration

```toml
# Before
moq-lite = "0.16"

# After
moq-net = "0.1"
```

```rust
// Before
use moq_lite::{Session, Broadcast, Track};

// After
use moq_net::{Session, Broadcast, Track};
```
