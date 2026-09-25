# [M] The SETUP AUTHORIZATION TOKEN option reaches the verifier

## Goal

A moq-transport peer's SETUP `AUTHORIZATION TOKEN` option (key `0x03`,
draft-14 through the newest supported draft) is decoded by `rs/moq-net` into
a token type and value, exposed on the accepted handshake so an app can run
its own verifier, and forwarded by the relay in `moq_auth::Request` as
`token`, so an auth server can verify it. Today the option is stored as
opaque bytes and ignored.

Boundaries: SETUP only. The same parameter on SUBSCRIBE and every other
message is [Request tokens](/quest/m2/cat/request-token.md)'s refusal; renewal
through REQUEST_UPDATE and a non-zero EXPIRES is the
[in-band auth](/quest/m1/auth/README.md) line's AUTH exchange, not a
per-request token. No client configuration: [Token in
band](/quest/m1/auth/token-in-band.md) owns presenting one.

## Plan

- `moq_net::setup::Token { kind: u64, value: Vec<u8> }`, `kind` being the
  wire Token Type: `Token::OUT_OF_BAND` is `0x0` and `Token::CAT` is the
  `0x01` c4m-01 registers. `setup` becomes a public module exporting only
  `Token`; the SETUP wire types stay crate-private. Decode the draft-21
  section 8.9 structure: Alias Type `USE_VALUE` yields the token; `REGISTER`
  is treated as `USE_VALUE` because we advertise no
  `MAX_AUTH_TOKEN_CACHE_SIZE` (the default of 0 makes that the draft's own
  rule), so no alias state exists; `DELETE` or `USE_ALIAS` in SETUP closes
  with `PROTOCOL_VIOLATION`; an undecodable structure closes with
  `KEY_VALUE_FORMATTING_ERROR`. More than one token in SETUP is refused: one
  credential per connection is the shape the contract has.
- Encode side: `Parameters` learns to write a `USE_VALUE` token, so [Token
  in band](/quest/m1/auth/token-in-band.md) and
  [Present](/quest/m2/cat/present.md) have nothing to add on the wire.
- `moq_net::server::Handshake::token() -> Option<&setup::Token>` beside
  `path()` and `role()`, carried through the `Legacy` (draft-14 to 16) and
  `PeerSetup` (draft-17+) paths; `moq_tokio::server::Request::token()`
  forwards it; lite sessions return `None`.
- `moq_auth::Request.token: Option<Token { kind: u64, value: base64 }>`,
  serialized with `serde_with` base64 like the rest of the request. The relay
  fills it in `request_for` in `rs/moq-relay/src/auth.rs`, beside the URL
  query it already forwards. `moq auth serve` treats kind `0x0` as the JWT
  the deployment negotiated out of band, verified exactly like `?jwt=`, and
  refuses any other kind naming the code until
  [Verify](/quest/m2/cat/verify.md) teaches it `0x01`. A request carrying
  both a SETUP token and a `jwt` query is refused naming both.
- `js/net/src/ietf/parameters.ts` mirrors the decode and encode rules. No JS
  accept-side API: nothing in `js/net` authorizes an IETF session.
- Docs: `doc/concept` on the IETF binding gains the option;
  `doc/lib/rs/moq-auth.md` gains the request field; `doc/bin/relay/auth.md`
  says the relay forwards the option and `moq auth serve` verifies a type-0
  token like `?jwt=`.
- Tests: decode and encode round trips for every alias type on every draft
  in Rust and JS; `DELETE`/`USE_ALIAS` in SETUP close the session; `REGISTER`
  is accepted as a value; two tokens in SETUP are refused; the handshake
  exposes the bytes on one legacy and one draft-17+ session and a lite
  session reports none; the relay forwards the bytes to a wiremock auth
  server byte for byte; `moq auth serve` admits a type-0 JWT, refuses an
  unknown kind, and refuses a token plus `?jwt=`.

Public API: additive on `moq-net`, `moq-tokio`, `moq-auth`, and `js/net`.
Wire: none new; the option already exists in every supported draft.

## Related

- [Token in band](/quest/m1/auth/token-in-band.md) - writes the same option
  from the client side with the shared `setup::Token`
- [Common Access Tokens](/quest/m2/cat/README.md) - verifies and presents a
  CAT through this option
