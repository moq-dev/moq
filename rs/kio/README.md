<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/kio/badge.svg)](https://docs.rs/kio/)
[![Crates.io](https://img.shields.io/crates/v/kio.svg)](https://crates.io/crates/kio)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# kio

Producer/consumer shared state with async waker-based notification.

This crate provides `Producer` and `Consumer` types that share state through a mutex-protected value.
Producers can modify the state and consumers are automatically notified via async wakers.
The channel auto-closes when all producers are dropped.

`Shared` is the role-less sibling for state that both sides mutate, and `Queue` is a
poll-native FIFO queue (bounded or unbounded) built in the same style. `Park` holds a
waiter for as long as a poll stays pending, bridging a `std::task::Context` to kio's
waiter-based polls when implementing `Future` or `poll_*` on top of kio channels.

It's used internally by [moq-net](https://github.com/moq-dev/moq/tree/main/rs/moq-net) and friends, but is generic enough to be useful on its own.

See the [API documentation](https://docs.rs/kio/) for details.
