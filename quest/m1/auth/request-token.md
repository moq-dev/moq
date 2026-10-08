# [S] A token on a request decodes by the draft's rules

## Goal

The `AUTHORIZATION TOKEN` parameter (`0x03`) on a moq-transport request
(SUBSCRIBE, REQUEST_UPDATE, PUBLISH, FETCH, PUBLISH_NAMESPACE,
SUBSCRIBE_NAMESPACE, TRACK_STATUS, or any other request that carries
parameters) decodes with the same structure and rules as the SETUP option.
A token the draft says must close the session closes it with the draft's
code; a `USE_VALUE` token is read and, for now, grants nothing, so a request
outside the session grant is still refused `UNAUTHORIZED`. Today every
request decodes the key and ignores it.

## Plan

Decided 2026-10-08: this quest is the decode-and-close change only. A
request token authorizing its own request (the fallback after the session
grant, refresh by REQUEST_UPDATE, and a per-request relay lease on
`moq_auth::Client`) is deferred until a moq-transport peer needs it; re-plan
it then from [#4675](https://github.com/moq-dev/moq/pull/4675), which built
it. #4675 is too large to review as one change, so it splits: the
decode-and-close part lands first as this quest, and the grant and lease work
parks on its branch. The decode needs neither [Relay tokens](/quest/m1/auth/relay-refresh.md)
nor a lease.

- Decode with the SETUP option's structure and rules
  (`rs/moq-net/src/ietf/token.rs`, `js/net/src/ietf/token.ts`): `USE_VALUE` yields the token. `REGISTER` closes the session with
  `AUTH_TOKEN_CACHE_OVERFLOW` (0x13): we
  advertise no `MAX_AUTH_TOKEN_CACHE_SIZE`, so the limit is 0, and draft-21
  §8.9 says overflowing it MUST close the session. Only SETUP falls back to
  USE_VALUE (§9.1.4). `DELETE` or `USE_ALIAS` closes with
  `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17): a cache size of 0 "prohibits the use
  of token Aliases" (§9.1.3), so no alias is ever registered. §8.9 says to
  reject the message, but 0x17 exists only as a Session Termination Code
  (§12.2), not a Request Error Code. Record that close as a deviation
  under "moq-transport" in `doc/concept/standard.md`. A token structure that does not
  decode closes with `KEY_VALUE_FORMATTING_ERROR`. Paul Gregoire's
  validator checks the REGISTER and malformed-token codes (decided
  2026-10-04; aliases decided 2026-10-05). Both decoder families change: the strict
  `decode_params!` path, where each request reads the repeatable key into an
  ignored `Vec<Opaque>`, and draft-14's `Parameters::skip`, which consumes it
  unread. `js/net` keeps every instance in `Parameters` and reads none.
- Build on #5028's per-draft parameter decode (merged), which reworked the
  same per-message decode.
- 0x13 and 0x17 join the shared session registry: `SessionError`
  (`rs/moq-net/src/error.rs`) and `SessionCode` (`js/net/src/error.ts`).
  Lite codes below 32 carry moq-transport's meaning, so add both rows to the
  Session Error Codes table in `drafts/draft-lcurley-moq-lite.md` and to
  `session_codes_round_trip`, as [Malformed moq-transport
  input](/quest/m2/ietf-malformed-close.md) does for its codes. No
  per-version mapping. That adds two codes to moq-lite's wire registry.
- `js/net` mirrors the decode.
- Tests, one legacy and one strict draft, Rust and JS: a `REGISTER` token on
  a request closes with `AUTH_TOKEN_CACHE_OVERFLOW`, a `DELETE` or
  `USE_ALIAS` one with `UNKNOWN_AUTH_TOKEN_ALIAS`, a malformed request token
  with `KEY_VALUE_FORMATTING_ERROR`, and a `USE_VALUE` token on a request
  outside the session grant is still refused `UNAUTHORIZED`.

Public API: none beyond the two session codes. Wire: session codes 0x13 and
0x17 are named; the parameter already exists in every supported draft.

## Related

- [Relay tokens](/quest/m1/auth/relay-refresh.md) - the lease a deferred per-request grant would reuse
