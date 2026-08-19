---
title: moq-tokio
description: Tokio-based connection helpers for native Rust apps
---

# moq-tokio

[![crates.io](https://img.shields.io/crates/v/moq-tokio)](https://crates.io/crates/moq-tokio)
[![docs.rs](https://docs.rs/moq-tokio/badge.svg)](https://docs.rs/moq-tokio)

Tokio-based connection helpers for native Rust applications. Provides TLS configuration, certificate management, and connection establishment utilities used by the relay server and CLI tools.

## Overview

`moq-tokio` bridges the gap between the transport-agnostic `moq-net` crate and actual QUIC/WebTransport networking. It handles:

- TLS certificate loading and configuration
- QUIC connection setup via a pluggable backend, defaulting to [quinn](https://crates.io/crates/quinn), with noq and quiche available through features
- WebTransport session management
- Development certificate generation for local testing
- Thread-per-core QUIC workers (`worker::Workers`): a pinned thread and socket per core, sharing one port and steered by connection ID

## Installation

```toml
[dependencies]
moq-tokio = "0.19"
```

## API Reference

Full API documentation: [docs.rs/moq-tokio](https://docs.rs/moq-tokio)

## Next Steps

- Build with [moq-net](/lib/rs/crate/moq-net) for the core pub/sub protocol
- Deploy a [relay server](/bin/relay/)
