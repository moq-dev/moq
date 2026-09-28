# [XS] IPv6 literal TLS names on every transport

## Goal

Dialing an IPv6 literal (`wss://[::1]:4443`, `moqt://[::1]`) with no
`tls_host_name` override completes the TLS handshake on every moq-tokio
transport. [#4322](https://github.com/moq-dev/moq/pull/4322) fixed the noq
QUIC path, where `url::Host::to_string()` kept the URL brackets and rustls
refused `[::1]` as a server name. The WebSocket path likely has the same bug:
`websocket.rs` takes its host from `url.host_str()`, which also keeps the
brackets.

## Plan

- Reproduce on WebSocket with a loopback `[::1]` listener and no override,
  like #4322's pinned QUIC tests, before fixing.
- Check every place moq-tokio derives a TLS server name from a URL (WebSocket,
  TCP, iroh, and any other dialer) and derive it the same way everywhere. A
  shared helper is reasonable if more than one site needs it.
- Keep the bracketed form wherever it is correct: the HTTP `Host` header and
  URL rebuilding.

Public API and wire: none.
