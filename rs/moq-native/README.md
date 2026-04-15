<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-native/badge.svg)](https://docs.rs/moq-native/)
[![Crates.io](https://img.shields.io/crates/v/moq-native.svg)](https://crates.io/crates/moq-native)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-native

Helper library for native [Media over QUIC](https://moq.dev) applications, on top of [moq-lite](https://github.com/moq-dev/moq/tree/main/rs/moq-lite).

Establishes MoQ connections over a few different transports, selectable via cargo features:

- **WebTransport** (HTTP/3) via [quinn](https://crates.io/crates/quinn) (default) or [quiche](https://crates.io/crates/quiche)
- **Raw QUIC** with ALPN negotiation
- **WebSocket** as a fallback when QUIC isn't available
- **Iroh** P2P (`iroh` feature)

Also handles TLS, certificate generation, logging setup, and reconnection logic, with `clap`-derived configuration ready for binaries.

## Examples

- [Publishing a chat track](examples/chat.rs)

See the [API documentation](https://docs.rs/moq-native/) for details.
