# [M] The auth request reports what the connection negotiated

## Goal

The auth request reports what the connection actually negotiated, and says which of it the client chose:

- `moq_auth::Request::server_name` is the SNI that selected the relay's certificate on every transport, as its contract says, not the client's `Host` or `:authority`. The SNI is still client-chosen, among the names the relay serves, and the doc says so: a hostname-scoped auth server binds tenancy to the token, not to `server_name` alone.
- A verified client certificate stays a fact unless the auth server's grant says `peer`; it never reveals hidden routes on its own.

Today `admit_http` fills `server_name` from `request_host(uri, headers)`, and the WebSocket bridge builds its URL from the same header. A client can therefore report a host the connection never negotiated TLS for. `cluster_peer` also treats any certificate as a peer for hidden-route discovery.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add `server_name()` to the `HttpStream` trait and return `TlsStream::get_ref().1.server_name()`; use it in `admit_http` and in the WebSocket session's link. On the plain-HTTP listener leave `server_name` absent rather than filling it from `Host`, since the auth request has no field that would tell an auth server the value is client-chosen; say so in the doc.
- Remove the same fallback from WebTransport on both listener stacks: `request_for` substitutes the request authority (`rs/moq-relay/src/auth.rs`) and the io_uring path the URL host (`rs/moq-relay/src/uring.rs`) when the client sent no SNI.
- Derive hidden-route visibility from the grant's `peer` flag, not `request.tls.is_some()`. Until peers set it, document the exemption where operators read it.
- Bound the `moq_auth::Client` reply body.
- Update `doc/bin/relay/auth.md` so `server_name` (the certificate-selecting SNI, client-chosen), `peer`, and the certificate-is-a-fact rule match what the code does.

Tests: an HTTP/1.1 request with a forged `Host` still reports the SNI; the same over WebSocket; a WebTransport session without SNI reports none on both stacks; a certificate-only session sees no hidden routes.

Public API: one method on an internal trait. Wire: none.

## Related

- [In-band auth](/quest/m1/auth/README.md) - the request and grant contract this corrects
- [moq-dev/moq#6384](https://github.com/moq-dev/moq/pull/6384) - the auth client refuses redirects
