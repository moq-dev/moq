---
title: moq-native
description: QUIC/WebTransport connection helpers for native Rust apps
---

# moq-native

[![crates.io](https://img.shields.io/crates/v/moq-native)](https://crates.io/crates/moq-native)
[![docs.rs](https://docs.rs/moq-native/badge.svg)](https://docs.rs/moq-native)

QUIC and WebTransport connection helpers for native Rust applications. Provides TLS configuration, certificate management, and connection establishment utilities used by the relay server and CLI tools.

## Overview

`moq-native` bridges the gap between the transport-agnostic `moq-lite` crate and actual QUIC/WebTransport networking. It handles:

- TLS certificate loading and configuration
- QUIC connection setup via [quinn](https://crates.io/crates/quinn)
- WebTransport session management
- Development certificate generation for local testing

## Installation

```toml
[dependencies]
moq-native = "0.1"
```

## API Reference

Full API documentation: [docs.rs/moq-native](https://docs.rs/moq-native)

## Next Steps

- Build with [moq-lite](/rs/crate/moq-lite) for the core pub/sub protocol
- Deploy a [relay server](/app/relay/)
