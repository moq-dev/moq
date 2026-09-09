# [M] Token in band

## Goal

A client's credential travels inside the session instead of the URL. Every
client in this repository connects without `?jwt=`, presents the token as its
first AUTH, and the relay admits the session on the anonymous grant and widens
it when the token verifies. The URL query keeps working as a legacy path so
existing deployments and the HTTP endpoints are unaffected, and the token no
longer appears in access logs, browser history, or the WebSocket fallback's
request line.

## Plan

- Client configuration separates the credential from the address:
  `moq_native::ClientConfig` gains `token`, the `moq` CLI a `--token` and
  `MOQ_TOKEN` beside the URL, moq-ffi `MoqClient::set_token`, libmoq
  `moq_client_set_token` (mirrored in the wrappers and `cpp/obs/src`), and
  `js/net`'s `connect` an options field. A token in both places is a
  configuration error, refused at connect.
- moq-net presents it: a lite session sends the configured token as its first
  AUTH instead of the empty one; a moq-transport session sends it as the
  AUTHORIZATION TOKEN setup option (`ParameterBytes::AuthorizationToken`,
  `USE_VALUE`, token type 0) and still opens the AUTH stream with an empty
  token to learn its grant.
- The relay admits first, scopes second. An anonymous connection today is
  admitted with the public grant when one is configured and refused
  otherwise; with this quest the refusal waits for the first AUTH, bounded by
  a short deadline after SETUP, so a client that never presents anything is
  still refused and a token-bearing one is scoped by the existing widening
  path from [Relay refresh](/quest/m2/auth/relay-refresh.md). A token in the
  setup option scopes at accept, as the URL does. `AuthParams::from_url` and
  `from_path_query` stay for the legacy query.
- The WebSocket 403 in [Connect auth race](/quest/m0/3532-connect-auth-race.md)
  becomes an in-band AUTH_ERROR on the QUIC arm as well; the race keeps its
  auth-only semantics.
- Docs: `doc/bin/cli.md`, `doc/bin/relay/auth.md` (legacy query, the
  admission deadline), `doc/lib/*` client configuration, and every example
  invocation that carries `?jwt=` in `doc/`, `demo/`, and the READMEs.
- Tests: a client with a token and no query is scoped exactly as the URL
  variant; an anonymous client with no public grant is refused at the
  deadline; a token in both places is refused at connect; the cross-language
  harness runs with tokens in band.

On main, additive.

## Required

- [Relay refresh](/quest/m2/auth/relay-refresh.md) - supplies the verify and
  widen path the first AUTH reuses
- [Bindings](/quest/m2/auth/bindings.md) - supplies the client surface the new
  token setters sit beside
