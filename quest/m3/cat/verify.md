# [M] moq auth serve admits a Common Access Token

## Goal

`moq_auth::cat` verifies a CAT (CTA-5007-B, a CWT under COSE_Mac0 or
COSE_Sign1) and turns its `moqt` and `moqt-reval` claims into a `Grant`, and
`moq auth serve` admits a connection whose SETUP token is one, with keys the
operator hands it. `moq auth sign --format cat` mints one and `moq auth
verify --format cat` checks one, so tests and operators have a way to make
tokens. Every claim we do not evaluate refuses the token naming the claim.

## Plan

- Crates: `coset` for COSE and CWT claims, `ciborium` for CBOR; HMAC through
  the `aws-lc-rs` the crate already links, ES256 through `p256`. Keys reuse
  `moq_auth::jwt::Key` files: a JWK with `alg` maps to the COSE algorithm
  (`HS256` to HMAC 256/256, `ES256` to -7, `EdDSA` to -8, `RS256` to -257);
  a JWK whose algorithm has no COSE mapping is refused at load naming it.
- `cat::Claims { issuer, audience, subject, expires, not_before, issued,
  id, scopes: Vec<cat::Scope>, revalidate: Option<Duration> }` where
  `cat::Scope { actions: Vec<cat::Action>, namespace: cat::Match, track:
  cat::Match }` and `cat::Match` is `Any`, `Exact(bytes)`, `Prefix(bytes)`,
  `Suffix(bytes)`, or `Contains(bytes)`. `cat::Action` is the c4m table 1
  enum. Claim keys `moqt` and `moqt-reval` are provisional constants in the
  CWT private-use range with a doc comment; they change when the draft
  registers them.
- `Claims::verify(token, keys)` checks the signature or MAC against the key
  set by `kid`, then `exp`, `nbf`, and `iat` against now, then walks the
  claim set: anything outside `iss`, `sub`, `aud`, `exp`, `nbf`, `iat`,
  `cti`, `moqt`, and `moqt-reval` refuses the token naming the label.
- `Claims::grant(&self, root) -> Result<Grant>`: for every scope, actions
  `ANNOUNCE` and `PUBLISH` add the namespace to `publish`, `SUBSCRIBE`,
  `SUBSCRIBE_NAMESPACE`, `SUBSCRIBE_UPDATE`, `FETCH`, and `TRACK_STATUS` add
  it to `subscribe`, `CLIENT_SETUP` and `SERVER_SETUP` add nothing. The
  namespace match becomes a `moq_pattern::Pattern` relative to the dialed
  path, which is the token's root exactly as `jwt::Claims::root` is: `Any`
  is `**`, `Exact` is the literal, `Prefix` ending on a segment boundary
  (empty or trailing `/`) is `prefix/**`, any other `Prefix`, `Suffix`, or
  `Contains` is refused naming the scope. A track match other than `Any` is
  refused naming the scope. `exp` is `expires`, `moqt-reval` is
  `revalidate`, and `Grant::validate` keeps refusing a cadence without an
  expiry, so the serve default `--expires` applies to a token without `exp`
  the way it does for a JWT. A token that grants nothing is refused.
- `moq auth serve`: `--cat-key <file>` and `--cat-key-dir <dir>` beside
  `--key`, `--cat-issuer <name>` and `--cat-audience <name>` required to
  match `iss` and `aud` when set. Policy order: a SETUP `token` is decided
  first and never falls through; then the existing `jwt` query, mTLS,
  public. A request carrying both a SETUP token and a `jwt` is refused
  naming both.
- `moq auth sign --format cat` and `verify --format cat` in `rs/moq-cli`,
  sharing `--publish`/`--subscribe` patterns with the JWT form and emitting
  one `moqt` scope per pattern (`foo/**` becomes a prefix match on `foo/`, a
  literal an exact match, `**` an empty match) with the actions each verb
  implies; `--revalidate <duration>` sets `moqt-reval`. Output is the raw
  CWT bytes, or base64url with `--base64`.
- Docs: `doc/lib/rs/moq-auth.md` gains the module, `doc/bin/cli.md` the
  flags, `doc/bin/relay/auth.md` a CAT section with the provisional codes
  and the refused-claims list.
- Tests: sign and verify under each mapped algorithm; each refused claim;
  each match kind mapped or refused; expiry and not-before; the grant
  produced from the c4m examples; serve admitting a CAT over a real
  moq-transport session on every supported draft and refusing an unknown
  token kind, a bad MAC, and a token plus `jwt`; the CLI round trip.

Public API: `moq_auth::cat` new, `moq auth serve` and `moq auth sign|verify`
gain flags. Wire: none.

## Required

- [Package](/quest/m1/auth/package.md) - supplies `moq_auth::jwt`, which
  `cat` sits beside
- [Setup token](/quest/m3/cat/setup-token.md) - the token reaches the
  server's request
- [Serve](/quest/m1/auth/serve.md) - the server this extends
