# [M] Token in band

## Goal

Several credentials can ride one connection. Every client in this repository
takes tokens as configuration separate from the address, puts the first in
its SETUP (the URL query when the negotiated version has no SETUP token),
presents each other one on its own AUTH stream at setup, and the relay admits
the session on that first token and widens it as the streams are answered. Peers without the AUTH stream keep
working unchanged, so nothing is refused and nothing goes silent, and a
session's first requests wait on no AUTH.

## Plan

IETF AUTH initially carries only prefix-representable grants. Propagate its
`Unsupported` result for an unrepresentable pattern union; do not retry the
token through the URL, widen it, or leave token admission pending. Lite AUTH
already carries the full pattern grant.

- Client configuration separates the credential from the address: `moq_tokio::connect::Config` gains `tokens`, repeatable as
  `--connect-token` and `MOQ_CONNECT_TOKEN`, the default set for every dial
  the client makes. A `?jwt=` in the URL stays a member of the union, which is
  how cluster dial targets keep their per-peer credential, and per-dial
  extras use the session's `auth().add()` once connected. moq-ffi gains a
  `tokens` field on the `MoqClientConfig` record that
  the FFI shape (#4697) put in place of every
  `MoqClient` setter, mirroring `moq_tokio::connect::Config`, not a new
  `set_tokens` setter (mirrored in the wrappers, and reaching the
  generated C and C++ bindings and OBS through the generated C++) and `js/net`'s `connect`
  options field follow. Decided in the 2026-09-30 audit: no new libmoq API,
  since it gets no more feature work.
- Credential refresh belongs with token presentation. Resolve the configured
  credential source before each dial. After an authorization refusal, resolve
  it once more and retry only when the credential changed; do not loop on a
  rejected credential. On an AUTH-capable session, replace a token through
  its own AUTH stream so the accepted grant union remains live. Keep `?jwt=`
  as the fallback for peers without AUTH. Choose the public credential-source
  API while implementing this quest.
- Presenting: ✅ Decided 2026-10-19 (maintainer,
  [#5972](https://github.com/moq-dev/moq/pull/5972)): the first configured
  token rides SETUP. lite-07-wip gains a SETUP token parameter, filling
  `server::Handshake::token` the way moq-transport's AUTHORIZATION TOKEN
  setup option (`ietf::token::into_setup`, `USE_VALUE`, token type 0) already
  does, so the relay hands it to the auth server at accept. It costs no round
  trip: the acceptor already reads the client's SETUP before serving, and a
  lite client sends requests without waiting on the server's SETUP. The
  version is negotiated (ALPN or the WebTransport subprotocol) before SETUP
  is written, but the URL is fixed before negotiation, so the client also
  copies the first token into the URL query while any offered version lacks
  a SETUP token (every lite version before 07). On raw QUIC and Qmux the
  query already rides SETUP's `Path`. A dial URL that already carries a
  credential (a cluster's per-peer `?jwt=`) keeps it as the connection
  credential instead, and then every configured token gets its own AUTH
  stream: the client never appends a second `jwt` or sends a different SETUP
  token. A token that went into the URL or SETUP is the connection
  credential, and the empty-token stream is its one and only stream; every
  other configured token gets its own AUTH stream on an AUTH-capable session,
  so no token is ever granted twice. While the URL also carries it, the auth
  server sees the same credential twice: `moq auth serve` admits a SETUP
  token equal to the `?jwt=` value and refuses two different ones
  (`serve::Refusal::TwoTokens`). Never send different values in the two
  places. A token in SETUP is bounded by SETUP's Message Length cap and
  reaches the auth server exactly as a URL token does, so an unauthenticated
  client gains nothing it could not already send in the URL. Dropped: the
  earlier "always in the CONNECT request" rule and its auth-header plan.
- The relay admits on the URL, then widens. An anonymous connection is
  admitted with the public grant when one is configured and refused at
  connect otherwise, as today: a client with tokens always carries one in
  SETUP or the URL, so the relay needs no admission deadline. The URL query
  and the SETUP token stay the connect-time credentials in the
  `moq_auth::Request`.
- A URL credential is still refused at connect when both arms reject it, and
  an in-band refusal is an AUTH_ERROR after connect.
- Docs: `doc/bin/cli.md`, `doc/bin/relay/auth.md` (that the URL token is one
  member of the union), `doc/lib/*` client
  configuration, and the example invocations carrying `?jwt=` in `doc/`,
  `demo/`, and the READMEs, keeping the URL form documented as the way to
  reach an older relay.
- Spec the SETUP token parameter in `drafts/draft-lcurley-moq-lite.md`'s
  lite-07-wip SETUP, and update `js/net` beside Rust.
- Tests: a client with a configured token offering only lite-07 against an
  AUTH-capable relay is scoped exactly as the URL variant, with the token in
  SETUP and none in the URL; the same client against a lite-05 relay still
  authenticates through the URL; a moq-transport client offering a draft
  without AUTH, carrying the token in both the URL and the setup option, is
  admitted by `moq auth serve`; two configured tokens union; the
  cross-language harness runs with tokens configured.

Additive. Wire: a lite-07-wip SETUP parameter.

## Required

- [Bindings](/quest/m1/auth/bindings.md) - supplies the client surface the new
  token setters sit beside
