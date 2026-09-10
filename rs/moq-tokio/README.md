<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-tokio/badge.svg)](https://docs.rs/moq-tokio/)
[![Crates.io](https://img.shields.io/crates/v/moq-tokio.svg)](https://crates.io/crates/moq-tokio)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-tokio

Tokio-based connection helpers for native [Media over QUIC](https://moq.dev) applications, on top of [moq-net](https://github.com/moq-dev/moq/tree/main/rs/moq-net).

Establishes MoQ connections over a few different transports, selectable via cargo features:

- **WebTransport** (HTTP/3) via [noq](https://crates.io/crates/noq) (default), [quinn](https://crates.io/crates/quinn), or [quiche](https://crates.io/crates/quiche)
- **Raw QUIC** with ALPN negotiation
- **WebSocket** as a fallback when QUIC isn't available
- **Iroh** P2P (`iroh` feature)

Also handles TLS, certificate generation, logging setup, and reconnection logic, with Usage-derived configuration ready for binaries.

## Examples

- [Publishing a chat track](examples/chat.rs)

See the [API documentation](https://docs.rs/moq-tokio/) for details.

## Fixed destinations

`connect::Addr::pinned(url, addresses)` supplies fixed socket addresses while
keeping the original URL for TLS verification and the request authority. Pass the
result to `Client::connect`, or combine it with other destinations using `Addrs`.
The client races the supplied addresses on its existing endpoint; WebSocket
fallback uses the same fixed addresses without another DNS lookup.

The constructor returns `connect::Error` for an empty list, unsupported scheme, or
missing host. Fixed
destinations support `https`, `wss`, `moqt`, and `moql`. Reconnects retain the fixed
addresses, and accepted peer redirects terminate with `Error::PinnedRedirect`,
including in one-shot mode. With mixed `Addrs`, the target that establishes the
session determines its redirect policy; unused pinned fallbacks do not restrict it.
To refresh DNS, resolve and apply your address policy again before creating a new
connection. Address filtering belongs to the caller.
