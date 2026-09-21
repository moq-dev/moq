# [S] A CAT is one kind of configured token

## Goal

`moq_tokio::connect::Config.tokens` and `js/net`'s connect `tokens` accept a
Common Access Token beside the JWTs they already hold, and a moq-transport
session carries it in the SETUP `AUTHORIZATION TOKEN` option with Token Type
`0x01`. The library does not read the bytes. A CAT never rides the URL or an
AUTH stream, so a client that configures one while offering a version without
the option fails loud instead of dropping the credential.

## Plan

- [Token in band](/quest/next/auth/token-in-band.md) defines the client token
  configuration and writes the first configured token into the setup option
  as type 0. This quest makes the kind explicit: each configured token is a
  `moq_net::setup::Token { kind, value }`, a JWT keeps kind `0x0` and its
  URL and AUTH stream behavior, and a CAT is kind `0x01`. `--connect-token`
  keeps taking a JWT; `--connect-cat <base64url>` (and `MOQ_CONNECT_CAT`)
  adds a CAT. One CAT per connection: it is the connection credential, so a
  configured CAT takes the setup option and the JWT that would have gone
  there rides its AUTH stream instead.
- A CAT with any offered version that lacks the setup option (every lite
  version) fails `init` with `Unsupported` naming the token; `js/net`
  rejects the same way before dialing.
- `moq` CLI publish and subscribe get the flag through the shared connect
  config; `doc/bin/cli.md` and `doc/lib/rs/moq-net.md` gain it.
- Tests: the server request sees kind `0x01` and the bytes on every draft
  in Rust, JS, and across; a JWT still goes to the URL or AUTH stream when
  a CAT holds the option; a lite offer with a CAT refuses at init; end to
  end against `moq auth serve` with a CAT from `moq auth sign --format cat`.

Public API: additive on `moq-tokio` and `js/net`. Wire: none.

## Required

- [Setup token](/quest/future/cat/setup-token.md) - the server-side exposure
  and the shared `setup::Token`
- [Verify](/quest/future/cat/verify.md) - the server that admits the token the
  end-to-end test presents
- [Token in band](/quest/next/auth/token-in-band.md) - the token
  configuration and setup-option writer this adds a kind to
