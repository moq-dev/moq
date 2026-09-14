# [S] Clients present a token in SETUP

## Goal

A moq-tokio or js/net client hands the library token bytes and a kind, and
they ride the SETUP `AUTHORIZATION TOKEN` option of a moq-transport session.
The library does not know or care what the bytes are; the JWT keeps the URL.
Connecting with a token over a version that has no such option fails loud.

## Plan

- `moq_tokio::connect::Config.token: Option<moq_net::setup::Token>`, off by
  default, with the clap and env spelling the config already uses for
  optional fields (`--connect-token-kind`, `--connect-token <base64url>`).
  `moq_net::Client` writes it as a `USE_VALUE` option in CLIENT_SETUP on
  every IETF draft.
- When the negotiated version is moq-lite, `connect` fails with
  `Unsupported` naming the token before SETUP is sent; a caller that offers
  only lite versions gets the same error at `init`.
- `js/net` `ConnectProps.token?: { kind: bigint; value: Uint8Array }` with
  the same behavior in `handshake.ts`.
- `moq` CLI publish and subscribe accept the flags through the shared connect
  config; `doc/bin/cli.md` and `doc/lib/rs/moq-net.md` gain them.
- Tests: the server request sees the token on every draft in Rust, JS, and
  across; a lite-only offer refuses; end to end against `moq auth serve`
  with a CAT once [Verify](/quest/m3/cat/verify.md) lands, otherwise the
  wiremock server from the setup-token quest.

Public API: additive on `moq-tokio` and `js/net`. Wire: none.

## Required

- [Setup token](/quest/m3/cat/setup-token.md) - the encoder and the
  server-side exposure this uses
