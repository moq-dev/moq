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

- `moq_net::setup::Token { kind: u64, value: Vec<u8> }`, `kind` being the
  wire Token Type: `Token::OUT_OF_BAND` is `0x0` and `Token::CAT` is the
  `0x01` c4m-01 registers. This is the type [Token in
  band](/quest/m2/auth/token-in-band.md) writes for a configured JWT
  (`USE_VALUE`, type 0), so the two quests share one struct and one encoder;
  whichever lands first adds them. Decode the draft-21 section 8.9
  structure: Alias Type `USE_VALUE` yields the token; `REGISTER` is treated
  as `USE_VALUE` because we advertise no `MAX_AUTH_TOKEN_CACHE_SIZE` (the
  default of 0 makes that the draft's own rule), so no alias state exists;
  `DELETE` or `USE_ALIAS` in SETUP closes with `PROTOCOL_VIOLATION`; an
  undecodable structure closes with `KEY_VALUE_FORMATTING_ERROR`. More than
  one token in SETUP is refused: one credential per connection is the shape
  the contract has.
- Encode side: `Parameters` learns to write a `USE_VALUE` token so
  [Present](/quest/m3/cat/present.md) has nothing to add on the wire.
- Message parameter `0x03` on SUBSCRIBE, REQUEST_UPDATE, PUBLISH, FETCH,
  PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE, TRACK_STATUS, and every other
  message whose decoder reads parameters decodes and is refused with the
  request's error path (`Unsupported`, mapped through `to_code` for the
  draft) naming the parameter, so the session survives and the log says why.
  Both decoder families change: the strict `decode_params!` path on newer
  drafts, which rejects the key today, and the generic KVP path the legacy
  drafts use, which keeps an odd key and lets the caller ignore it.
- `moq_net::Request::token() -> Option<&setup::Token>` on the accepted
  request, carried through the `Legacy` (draft-14 to 16) and `PeerSetup`
  (draft-17+) paths; `moq_tokio::server::Request` forwards it; lite sessions
  return `None`.
- `moq_auth::Request.token: Option<Token { kind: u64, value: base64 }>`,
  serialized with `serde_with` base64 like the rest of the request. The relay
  fills it where it builds the request, beside the URL query it already
  forwards. `moq auth serve` treats kind `0x0` as the JWT the deployment
  negotiated out of band, verified exactly like `?jwt=`, refuses a kind it
  does not know naming the code, and hands `0x01` to
  [Verify](/quest/m3/cat/verify.md). A request carrying both a SETUP token
  and a `jwt` query is refused naming both.
- `js/net/src/ietf/parameters.ts` mirrors the decode and encode, and the
  `SetupOption.AuthorizationToken` handling in `handshake.ts`; the JS accept
  side exposes the token on its request the same way.
- Docs: `doc/concept` on the IETF binding gains the option and the refusal;
  `doc/lib/rs/moq-auth.md` gains the request field.
- Tests: decode and encode round trips for every alias type on every draft
  in Rust and JS; `DELETE`/`USE_ALIAS` in SETUP close the session;
  `REGISTER` is accepted as a value; a token on SUBSCRIBE is refused and the
  session continues, on one legacy draft and one strict draft; the relay forwards the bytes to a wiremock auth server
  byte for byte; a lite session reports none.

Public API: additive on `moq-net`, `moq-tokio`, `moq-auth`, and `js/net`.
Wire: none new; the option already exists in every supported draft.

## Required

- [Relay](/quest/m1/auth/relay.md) - the relay builds `moq_auth::Request`
  there; this quest adds one field to it

## Related

- [Token in band](/quest/m2/auth/token-in-band.md) - writes the same option
  from the client side with the shared `setup::Token`
