---
title: Standards
description: IETF drafts and protocol specifications
---

# Standards

MoQ is built on open standards and protocol specifications.

## Protocol Specifications

### moq-lite

The core pub/sub transport protocol.

**Specification:** [draft-lcurley-moq-lite](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html)

This defines:
- Broadcasts, tracks, groups, frames
- Message types and encoding
- Subscription and announcement flows
- Prioritization and reliability

### hang

Media-specific encoding/streaming protocol.

**Specification:** [draft-lcurley-moq-hang](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html)

This defines:
- Catalog format for track discovery
- Frame container format
- Codec requirements
- Media-specific grouping

### Use Cases

Document describing various MoQ use cases.

**Specification:** [draft-lcurley-moq-use-cases](https://moq-dev.github.io/drafts/draft-lcurley-moq-use-cases.html)

Covers:
- Live video streaming
- Audio conferencing
- Text chat and data streams
- Gaming and IoT

## Relationship to IETF MoQ

This project is a [fork](https://moq.dev/blog/transfork) of the [IETF MoQ Working Group](https://datatracker.ietf.org/group/moq/documents/) specification.

### Key Differences

| Aspect | IETF MoQ | This Project |
|--------|----------|--------------|
| Scope | Broad, general-purpose | Focused on deployability |
| Design | Feature-rich, many extensions | Minimal, opinionated |
| Status | Ongoing standardization | Production-ready |

### Why Fork?

The fork prioritizes:

1. **Simplicity** - Fewer concepts, easier to implement
2. **Deployability** - Works today with existing infrastructure
3. **Focus** - Optimized for live streaming use cases

Both efforts share knowledge and collaborate where beneficial.

## Underlying Standards

### QUIC

[RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000) - QUIC: A UDP-Based Multiplexed and Secure Transport

MoQ uses QUIC for:
- Stream multiplexing
- Built-in TLS 1.3
- Prioritization
- Congestion control

### WebTransport

[WebTransport over HTTP/3](https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/)

Provides browser access to QUIC features:
- Bidirectional streams
- Unidirectional streams
- Datagrams
- HTTP/3 handshake for firewall traversal

### WebCodecs

[W3C WebCodecs API](https://www.w3.org/TR/webcodecs/)

Browser API for media encoding/decoding:
- Hardware-accelerated codecs
- Low-level frame access
- Used by `@moq/hang`

### JWT

[RFC 7519](https://datatracker.ietf.org/doc/html/rfc7519) - JSON Web Token

Used for authentication:
- Path-based authorization
- Signed claims
- Expiration handling

## Contributing

Specifications are maintained in the [moq-dev/drafts](https://github.com/moq-dev/drafts) repository.

Contributions welcome:
- Issue feedback
- Propose clarifications
- Submit use cases
- Report implementation issues

## Next Steps

- Read the [Protocol details](/concept/layer/)
- Understand [Authentication](/app/relay/auth)
- Try the [Rust libraries](/rs/)
- Try the [TypeScript libraries](/js/)
