# [S] js/auth refuses the keys moq-auth refuses

## Goal

`@moq/auth` and `moq-auth` accept exactly the same JWKs and JWK Sets. Fuzzing the key
parsers found inputs only `@moq/auth` accepts, and `rs/moq-auth/tests/corpus.json` pins each
once it is refused.

- A JSON object that repeats a member, such as two `x` or `d` fields: `JSON.parse` keeps the
  last one, while serde refuses the key. Which value a key holds should never depend on
  which parser read it.
- A base64url field `moq-auth`'s decoder refuses, which `decodeBase64Flexible` takes anyway:
  `...aqE=` with nonzero trailing bits behind its padding, surrounding whitespace (it trims),
  and the standard alphabet's `+` and `/` (it maps them onto base64url).

## Plan

Recommended (agent, from the fuzz PR): make `@moq/auth` strict, as `moq-auth` is, rather
than loosen Rust. Failing loudly on a malformed key is the rule, and a key written by either
CLI never repeats a member or pads a field.

- Duplicate members: parse key JSON with a reviver or a small scanner that refuses a
  repeated key at any depth, in `Key.parse` and `KeySet.parse`.
- Base64url: decode key fields exactly as `moq-auth`'s `decode_base64url` does (unpadded,
  or padded with canonical trailing bits), and refuse the rest.
- Add one corpus case per kind under `jwk` with `"valid": false` (duplicate member,
  nonzero trailing bits, whitespace, standard alphabet); both languages replay the corpus.

Public API: none. Wire: none. Behavior: such keys stop loading in `@moq/auth`.
