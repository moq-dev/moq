# [M] The auth request reports the real SNI and nothing weaker

## Goal

The auth server decides from facts the client cannot choose:

- `moq_auth::Request::server_name` is the TLS SNI on every transport, as its contract says, not the client's `Host` or `:authority`.
- A verified client certificate stays a fact unless the auth server's grant says `peer`; it never reveals hidden routes on its own.
- The auth HTTP client cannot be redirected to another host with the session request in its body.

Today `admit_http` fills `server_name` from `request_host(uri, headers)`, and the WebSocket bridge builds its URL from the same header. A client can therefore tell a hostname-scoped auth server it dialed a tenant it never dialed. `cluster_peer` also treats any certificate as a peer for hidden-route discovery.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add `server_name()` to the `HttpStream` trait and return `TlsStream::get_ref().1.server_name()`; use it in `admit_http` and in the WebSocket session's link. Fall back to `Host` only on the plain-HTTP listener, and say so in the doc.
- Derive hidden-route visibility from the grant's `peer` flag, not `request.tls.is_some()`. Until peers set it, document the exemption where operators read it.
- Set `reqwest::redirect::Policy::none()` in `moq_auth::Client`, and bound the reply body. A 307/308 currently replays the credential-bearing request to wherever the auth server points.
- Update `doc/bin/relay/auth.md` so `server_name`, `peer`, and the certificate-is-a-fact rule match what the code does.

Tests: an HTTP/1.1 request with a forged `Host` still reports the SNI; the same over WebSocket; a redirect answer is refused; a certificate-only session sees no hidden routes.

Public API: one method on an internal trait, and a redirect policy. Wire: none.

## Related

- [In-band auth](/quest/m1/auth/README.md) - the request and grant contract this corrects
- [Focused release backports](/quest/m0/release-backports/README.md) - the auth client is shared with `release`, so check whether the redirect policy is a backport candidate
