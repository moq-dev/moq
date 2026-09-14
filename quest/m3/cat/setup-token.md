# [M] The SETUP AUTHORIZATION TOKEN option reaches the auth server

## Goal

A moq-transport peer's SETUP `AUTHORIZATION TOKEN` option (type `0x03`,
draft-14 through the newest supported draft) is decoded by `rs/moq-net` and
`js/net` into a token type and value, exposed on the accepted request, and
forwarded by the relay in `moq_auth::Request` as `token`, so an auth server
can verify it. The same parameter on any other message is refused. Today the
option is stored as opaque bytes and ignored, and the message parameter fails
decode as an unknown key.

## Plan

- `moq_net::setup::Token { kind: u64, value: Vec<u8> }` (`Token.kind` is
  the wire Token Type; `Token::CAT` is a provisional constant with a doc
  comment saying the IANA table is empty). Decode the draft-21 section 8.9
  structure: Alias Type `USE_VALUE` yields the token; `REGISTER` is treated
  as `USE_VALUE` because we advertise no `MAX_AUTH_TOKEN_CACHE_SIZE` (the
  default of 0 makes that the draft's own rule), so no alias state exists;
  `DELETE` or `USE_ALIAS` in SETUP closes with `PROTOCOL_VIOLATION`; an
  undecodable structure closes with `KEY_VALUE_FORMATTING_ERROR`. More than
  one token in SETUP is refused: one credential per connection is the shape
  the contract has.
- Encode side: `Parameters` learns to write a `USE_VALUE` token so
  [Present](/quest/m3/cat/present.md) has nothing to add on the wire.
- Message parameter `0x03` on SUBSCRIBE, PUBLISH, FETCH, PUBLISH_NAMESPACE,
  SUBSCRIBE_NAMESPACE, and TRACK_STATUS decodes and is refused with the
  request's error path (`Unsupported`, mapped through `to_code` for the
  draft) naming the parameter, so the session survives and the log says why.
- `moq_net::Request::token() -> Option<&setup::Token>` on the accepted
  request, carried through the `Legacy` (draft-14 to 16) and `PeerSetup`
  (draft-17+) paths; `moq_tokio::server::Request` forwards it; lite sessions
  return `None`.
- `moq_auth::Request.token: Option<Token { kind: u64, value: base64 }>`,
  serialized with `serde_with` base64 like the rest of the request. The relay
  fills it where it builds the request. `moq auth serve` refuses a token
  whose `kind` it does not know, naming the code; a known kind is
  [Verify](/quest/m3/cat/verify.md)'s.
- `js/net/src/ietf/parameters.ts` mirrors the decode and encode, and the
  `SetupOption.AuthorizationToken` handling in `handshake.ts`; the JS accept
  side exposes the token on its request the same way.
- Docs: `doc/concept` on the IETF binding gains the option and the refusal;
  `doc/lib/rs/moq-auth.md` gains the request field.
- Tests: decode and encode round trips for every alias type on every draft
  in Rust and JS; `DELETE`/`USE_ALIAS` in SETUP close the session;
  `REGISTER` is accepted as a value; a token on SUBSCRIBE is refused and the
  session continues; the relay forwards the bytes to a wiremock auth server
  byte for byte; a lite session reports none.

Public API: additive on `moq-net`, `moq-tokio`, `moq-auth`, and `js/net`.
Wire: none new; the option already exists in every supported draft.

## Required

- [Relay](/quest/m1/auth/relay.md) - the relay builds `moq_auth::Request`
  there; this quest adds one field to it
