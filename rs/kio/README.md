<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/kio/badge.svg)](https://docs.rs/kio/)
[![Crates.io](https://img.shields.io/crates/v/kio.svg)](https://crates.io/crates/kio)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)

# kio

Poll-native async shared state, a deterministic clock and simulator, and state-machine
macros.

Every wait is a `poll_*` method first, so the default build compiles no `async` code;
the `async` feature adds `async fn` wrappers that await them. Producers and consumers share state and park on predicates over it,
alongside a FIFO queue, an append-only log readers take entries from without a lock, and
a poll-driven task set. `clock` is the time those waits run on, and its `sim` feature
runs a test on a deterministic executor whose time jumps instead of sleeping. The
`model` feature checks a pure state machine by visiting every state it can reach. For
example (`cargo add --dev kio --features sim`):

```rust
use std::time::Duration;

// `#[kio::test]` runs an `async fn` test body this way.
kio::clock::sim::run(async {
    let start = kio::clock::now(); // Instant::ZERO
    kio::clock::sleep(Duration::from_secs(1)).await; // returns at once
    assert_eq!(kio::clock::now() - start, Duration::from_secs(1));
});
```

The [API documentation](https://docs.rs/kio/) tours each type and cargo feature, and
[MIGRATE.md](MIGRATE.md) says how to update your code across breaking changes.

It's used internally by [moq-net](https://github.com/moq-dev/moq/tree/main/rs/moq-net) and friends, but is generic enough to be useful on its own.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/moq-dev/moq/blob/main/LICENSE-APACHE) or
[MIT license](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT) at your option.
