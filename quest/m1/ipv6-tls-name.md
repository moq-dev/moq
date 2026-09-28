# [XS] IPv6 literal TLS names on every TLS dialer

## Goal

Dialing an IPv6 literal (`wss://[::1]:4443`, `moqt://[::1]`) completes the TLS
handshake on both moq-tokio TLS dialers, noq and WebSocket, with or without a
pinned `Addr` and with or without a `tls_host_name` override.
[#4322](https://github.com/moq-dev/moq/pull/4322) fixes noq, where
`url::Host::to_string()` kept the URL brackets and rustls refused `[::1]` as a
server name.

WebSocket likely already works without an override: every path, including the
pinned `connect_tls_override` one, hands tokio-tungstenite a bracketed URL, and
it strips the brackets before building the rustls name. The known gap is a bare
IPv6 override (`tls_host_name = "::1"`): `connect_tls_override` passes it to
`Url::set_host`, which rejects it, while noq accepts it.

## Plan

- Pin WebSocket with loopback `[::1]` tests like #4322's: unpinned and pinned
  `Addr` with no override, and a bare `::1` override. Fix only what fails.
- Derive the TLS server name the same way on both dialers. A shared helper is
  reasonable if both need it.
- Keep the bracketed form wherever it is correct: the HTTP `Host` header and
  URL rebuilding.
- TCP is plaintext and iroh dials by endpoint id, so neither is in scope.

Public API and wire: none.
