# Common Access Tokens

## Goal

A moq-transport client presents a Common Access Token
([draft-ietf-moq-c4m](https://datatracker.ietf.org/doc/draft-ietf-moq-c4m/))
in the SETUP `AUTHORIZATION TOKEN` option and the relay admits it with the
scope the token's `moqt` claim names, through the same
[auth server](/quest/m1/auth/README.md) contract every other credential
uses. Our own clients can present one; `moq auth serve` verifies one; the
relay stays crypto-free and forwards the bytes. Nothing changes on the
moq-lite wire or in the URL: a CAT rides the SETUP option only, and the JWT
stays the URL credential.

Boundaries decided while planning:

- SETUP only. A token on SUBSCRIBE, PUBLISH, FETCH, or any other request is
  refused; per-operation authorization is not planned. The relay authorizes
  per session through origin scopes, and a token that arrives after SETUP is
  the [in-band auth](/quest/m2/auth/README.md) line's problem.
- A token whose `moqt` scope restricts the track name is refused naming the
  token. Grants are broadcast-path patterns and tracks are not scoped.
- Core CWT claims plus `moqt` and `moqt-reval` are enforced; any other CAT
  claim present (`catu`, `catm`, `catnip`, `catreplay`, `catdpop`, `cnf`,
  geo, composite `or`/`and`/`nor`) refuses the token naming the claim, so a
  restriction we do not evaluate never widens silently. DPoP (c4m section 3)
  is out of scope.
- Provisional wire codes are hard-coded constants with a doc comment saying
  so: the IANA "MOQT Auth Token Type" table has only `0x0` reserved and the
  c4m claim keys are `TBD_MOQT`. We do not expect to interop on these until
  the drafts register values.
- Rust only. `@moq/auth` gains nothing; `js/net` only carries bytes it was
  handed into SETUP.
- [draft-ietf-moq-privacy-pass-auth](https://datatracker.ietf.org/doc/draft-ietf-moq-privacy-pass-auth/)
  was considered and dropped. A Privacy Pass token carries only the digest of
  its `TokenChallenge`, the draft never says how a relay hands a client the
  challenge whose `origin_info` scopes the token, VOPRF tokens need the
  issuer's secret at the verifier, and nobody is asking. Do not re-plan it
  until the draft has a challenge exchange and a registered token type.

## Plan

Order: the module split first so `cat` has a home beside `jwt`; the wire
next so a token reaches the auth server; verification; then our clients
present one. Everything requires the m1 auth line, which owns the `Request`
this rides and the server that answers it.

## Quests

- [Layout](/quest/m3/cat/layout.md) - the JWT moves under `moq_auth::jwt`
  so each token format is one module
- [Setup token](/quest/m3/cat/setup-token.md) - the SETUP `AUTHORIZATION
  TOKEN` option is decoded on both IETF stacks, reaches `moq_auth::Request`
  as `token`, and is refused on every other message
- [Verify](/quest/m3/cat/verify.md) - `moq_auth::cat` turns a CAT into a
  grant and `moq auth serve` admits one; `moq auth sign|verify` mint and
  check the format
- [Present](/quest/m3/cat/present.md) - moq-tokio and js/net clients carry a
  token into SETUP

## Related

- [Auth server](/quest/m1/auth/README.md) - the contract, the lease, and the
  server every quest here builds on
- [In-band auth](/quest/m2/auth/README.md) - credentials presented after
  SETUP, which this line refuses on the IETF wire
