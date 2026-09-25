# Common Access Tokens

## Goal

A moq-transport client presents a Common Access Token
([draft-ietf-moq-c4m](https://datatracker.ietf.org/doc/draft-ietf-moq-c4m/))
in the SETUP `AUTHORIZATION TOKEN` option and the relay admits it with the
scope the token's `moqt` claim names, through the same
`moq-auth` request contract every other credential uses. Our own clients can present one; `moq auth serve` verifies one; the
relay stays crypto-free and forwards the bytes. This is moq-transport only:
a CAT scopes itself by moq-transport message (`SUBSCRIBE`, `FETCH`,
`PUBLISH_NAMESPACE`, ...), which moq-lite has no equivalent for, so it rides
the SETUP option and nothing else. Nothing changes on the moq-lite wire or in
the URL, and the JWT stays the URL credential.

Boundaries decided while planning:

- SETUP only. A token on SUBSCRIBE, PUBLISH, FETCH, or any other request is
  refused; per-operation authorization is not planned. The relay authorizes
  per session through origin scopes, and a token that arrives after SETUP is
  the [in-band auth](/quest/m1/auth/README.md) line's problem.
- A token whose `moqt` scope restricts the track name is refused naming the
  token. Grants are broadcast-path patterns and tracks are not scoped.
- Core CWT claims plus `moqt` and `moqt-reval` are enforced; any other CAT
  claim present (`catu`, `catm`, `catnip`, `catreplay`, `catdpop`, `cnf`,
  geo, composite `or`/`and`/`nor`) refuses the token naming the claim, so a
  restriction we do not evaluate never widens silently. DPoP (c4m section 3)
  is out of scope.
- Wire codes follow c4m-01: Token Type `0x01` is the value the draft
  registers for CAT. The `moqt` and `moqt-reval` claim keys are still
  `TBD_MOQT`, so they are hard-coded constants with a doc comment saying so.
  We do not expect to interop on these until the draft registers them.
- Rust only. `@moq/auth` gains nothing; `js/net` only carries bytes it was
  handed into SETUP.
- [draft-ietf-moq-privacy-pass-auth](https://datatracker.ietf.org/doc/draft-ietf-moq-privacy-pass-auth/)
  was considered and dropped. A Privacy Pass token carries only the digest of
  its `TokenChallenge`, the draft never says how a relay hands a client the
  challenge whose `origin_info` scopes the token, VOPRF tokens need the
  issuer's secret at the verifier, and nobody is asking. Do not re-plan it
  until the draft has a challenge exchange and a registered token type.

## Plan

Order: the wire first so a token reaches the auth server, which is the
standalone [Setup token](/quest/m1/setup-token.md) quest; verification;
then our clients present one. Everything rides `moq_auth::Request` and
`moq auth serve`, which shipped on dev. The JWT types sit at the crate root;
the verify quest moves them under `moq_auth::jwt` so `cat` is a sibling
module rather than a set of prefixed names.

## Quests

- [Verify](/quest/m2/cat/verify.md) - `moq_auth::cat` turns a CAT into a
  grant and `moq auth serve` admits one; `moq auth sign|verify` mint and
  check the format
- [Present](/quest/m2/cat/present.md) - a CAT is one kind of configured
  token, riding the SETUP option the in-band token quest already writes
- [Request tokens](/quest/m2/cat/request-token.md) - the token parameter on
  any message but SETUP is refused with a request error

## Required

- [Setup token](/quest/m1/setup-token.md) - the SETUP option reaches
  `moq_auth::Request` as `token`

## Related

- [In-band auth](/quest/m1/auth/README.md) - credentials presented after
  SETUP, which this line refuses on the IETF wire; its [Token in
  band](/quest/m1/auth/token-in-band.md) quest owns the client token
  configuration a CAT joins
