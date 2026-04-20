<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/conducer/badge.svg)](https://docs.rs/conducer/)
[![Crates.io](https://img.shields.io/crates/v/conducer.svg)](https://crates.io/crates/conducer)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# conducer

Producer/consumer shared state with async waker-based notification.

This crate provides `Producer` and `Consumer` types that share state through a mutex-protected value.
Producers can modify the state and consumers are automatically notified via async wakers.
The channel auto-closes when all producers are dropped.

It's used internally by [moq-lite](https://github.com/moq-dev/moq/tree/main/rs/moq-lite) and friends, but is generic enough to be useful on its own.

See the [API documentation](https://docs.rs/conducer/) for details.
