# [M] Token in band

## Goal

A client's credentials can travel inside the session instead of the URL, and
several of them can ride one connection. Every client in this repository
takes tokens as configuration separate from the address, presents each on its
own AUTH stream at setup, and the relay admits the session on what the URL
carried and widens it as the streams are answered. Peers below lite-06 keep
working unchanged: the client puts the first token in the URL whenever any
version it offers has no AUTH stream, so nothing is refused and nothing goes
silent, and the token leaves the URL for good only once the offered set is
AUTH-capable.

## Plan

- Client configuration separates the credential from the address on dev's
  API: `moq_tokio::connect::Config` gains `tokens`, repeatable as
  `--connect-token` and `MOQ_CONNECT_TOKEN`, the default set for every dial
  the client makes. A `?jwt=` in the URL stays a member of the union, which is
  how cluster dial targets keep their per-peer credential, and per-dial
  extras use the session's `auth().add()` once connected. moq-ffi
  `MoqClient::set_tokens`, libmoq `moq_client_set_tokens` (mirrored in the
  wrappers and `cpp/obs/src`), and `js/net`'s `connect` options field follow.
- Presenting: WebTransport negotiates the moq version as a subprotocol of
  the CONNECT request that carries the URL, so the client cannot wait to
  learn whether the peer speaks AUTH. The client copies the first configured
  token into the URL query whenever any offered version lacks AUTH (today,
  everything below lite-06), and omits it once every offered version has the
  stream. A token that went into the URL or the setup option is the
  connection credential, and the empty-token stream is its one and only
  stream; every other configured token gets its own AUTH stream on an
  AUTH-capable session, so no token is ever granted twice. On moq-transport the first token also rides the AUTHORIZATION
  TOKEN setup option (`ParameterBytes::AuthorizationToken`, `USE_VALUE`,
  token type 0), which scopes at accept the way the URL does.
- The relay admits on the URL, then widens. An anonymous connection today is
  admitted with the public grant when one is configured and refused
  otherwise; with this quest a connection with no URL credential and no
  public grant is held for a short deadline after SETUP for its first
  accepted AUTH before being refused, so an AUTH-capable client with tokens
  only in band gets in and a client that never presents anything is still
  refused. `AuthParams::from_url` and `from_path_query` stay as the URL path.
- The WebSocket 403 in [Connect auth race](/quest/m0/3532-connect-auth-race.md)
  keeps its meaning: a URL credential is still refused at connect on either
  arm, and an in-band refusal is an AUTH_ERROR after connect.
- Docs: `doc/bin/cli.md`, `doc/bin/relay/auth.md` (the admission deadline and
  that the URL token is one member of the union), `doc/lib/*` client
  configuration, and the example invocations carrying `?jwt=` in `doc/`,
  `demo/`, and the READMEs, keeping the URL form documented as the way to
  reach an older relay.
- Tests: a client with a configured token against an AUTH-capable relay is
  scoped exactly as the URL variant and the URL carries the token only while
  an old version is offered; the same client against a lite-05 relay still
  authenticates through the URL; two configured tokens union; a client with
  in-band tokens only and no public grant is admitted, and one that presents
  nothing is refused at the deadline; the cross-language harness runs with
  tokens configured.

Branch from dev, or from main once [merge-dev](/quest/m1/merge-dev.md) lands.
Additive.

## Required

- [Relay tokens](/quest/m2/auth/relay-refresh.md) - supplies the verify and
  widen path the configured tokens reuse
- [Bindings](/quest/m2/auth/bindings.md) - supplies the client surface the new
  token setters sit beside
- [moq-transport](/quest/m2/auth/moq-transport.md) - supplies the IETF AUTH
  exchange the setup-option token pairs with
