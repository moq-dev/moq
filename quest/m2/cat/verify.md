# [M] moq auth serve admits a Common Access Token

## Goal

`moq_auth::cat` verifies a CAT (CTA-5007-B, a CWT under COSE_Mac0 or
COSE_Sign1) and turns its `moqt` and `moqt-reval` claims into a `Grant`, and
`moq auth serve` admits a connection whose SETUP token is one, with keys the
operator hands it. `moq auth sign --format cat` mints one and `moq auth
verify --format cat` checks one, so tests and operators have a way to make
tokens. Every claim we do not evaluate refuses the token naming the claim.

## Plan

The JWT types (`Claims`, `Key`, `Jwk`, `KeyId`, `Algorithm`, the key set,
`authorize`) sit at the `moq_auth` root today. Move them under
`moq_auth::jwt` first, keeping their names, so `cat` is a sibling module
rather than a set of prefixed names; `@moq/auth` stays flat.

- Crates: `coset` for COSE and CWT claims, `ciborium` for CBOR; HMAC through
  the `aws-lc-rs` the crate already links, ES256 through `p256`. Keys reuse
  `moq_auth::jwt::Key` files: a JWK with `alg` maps to the COSE algorithm
  (`HS256` to HMAC 256/256, `ES256` to -7, `EdDSA` to -8, `RS256` to -257);
  a JWK whose algorithm has no COSE mapping is refused at load naming it.
- `cat::Claims { issuer, audience, subject, expires, not_before, issued,
  id, scopes: Vec<cat::Scope>, revalidate: Option<Duration> }` where
  `cat::Scope { actions: Vec<cat::Action>, namespace: Option<Vec<cat::Match>>,
  exact_depth: bool, track: Option<cat::Match> }` follows the c4m-01 CDDL:
  `cat::Match` is `Exact(bytes)`, `Prefix(bytes)`, or `Suffix(bytes)`, one
  per namespace field in order, the trailing `nil` becomes `exact_depth`,
  and `nil` anywhere else refuses the token. `cat::Action` is the c4m-01
  table 1 enum (`ClientSetup`, `ServerSetup`, `PublishNamespace`,
  `SubscribeNamespace`, `Subscribe`, `RequestUpdate`, `Publish`, `Fetch`,
  `TrackStatus`). Claim keys `moqt` and `moqt-reval` are provisional
  constants in the CWT private-use range with a doc comment; they change
  when the draft registers them.
- `Claims::verify(token, keys)` checks the signature or MAC against the key
  set by `kid`, then `exp`, `nbf`, and `iat` against now, then walks the
  claim set: anything outside `iss`, `sub`, `aud`, `exp`, `nbf`, `iat`,
  `cti`, `moqt`, and `moqt-reval` refuses the token naming the label.
- `Claims::grant(&self, root) -> Result<Grant>`: for every scope,
  `Publish` adds the namespace pattern to `publish` and `Subscribe` adds it
  to `subscribe`. The grant has no narrower verbs, so `PublishNamespace` is
  accepted only beside `Publish`, and `SubscribeNamespace`, `RequestUpdate`,
  `Fetch`, and `TrackStatus` only beside `Subscribe`; a scope whose actions
  are narrower than either verb refuses the token naming the scope rather
  than widening a fetch-only token into a live subscription. `ClientSetup`
  and `ServerSetup` add nothing. The namespace fields become one `moq_pattern::Pattern` segment
  each, the mapping `rs/moq-pattern` documents under "CAT / C4M", relative
  to the dialed path exactly as `jwt::Claims::root` is: `Exact(f)` is the
  literal segment, `Prefix(f)` is `f*`, `Suffix(f)` is `*f`, a named
  namespace without `exact_depth` appends `/**`, and an absent namespace is
  bare `**` with nothing appended. A field value containing `/` or `*`
  cannot be a segment and refuses the token naming the scope. `f*` and `*f`
  are patterns the relay admits through the pattern scopes landed in
  [#3746](https://github.com/moq-dev/moq/pull/3746). A scope with a track
  match is refused naming the scope.
  `exp` is `expires`, `moqt-reval` is
  `revalidate`, and `Grant::validate` keeps refusing a cadence without an
  expiry, so the serve default `--expires` applies to a token without `exp`
  the way it does for a JWT. A token that grants nothing is refused.
- `moq auth serve`: `--cat-key <file>` and `--cat-key-dir <dir>` beside
  `--key`, `--cat-issuer <name>` and `--cat-audience <name>` required to
  match `iss` and `aud` when set. Policy order: a SETUP `token` is decided
  first and never falls through; then the existing `jwt` query, mTLS,
  public.
- `moq auth sign --format cat` and `verify --format cat` in `rs/moq-cli`,
  sharing `--publish`/`--subscribe` patterns with the JWT form and emitting
  one `moqt` scope per pattern through the inverse of the mapping above (a
  literal segment is `Exact`, `f*` is `Prefix`, `*f` is `Suffix`, a trailing
  `**` drops `nil`, a pattern with `**` elsewhere or `a*b` is refused naming
  it) with the actions each verb implies; `--revalidate <duration>` sets
  `moqt-reval`. Output is the raw CWT bytes, or base64url with `--base64`.
- Docs: `doc/lib/rs/moq-auth.md` gains the module, `doc/bin/cli.md` the
  flags, `doc/bin/relay/auth.md` a CAT section with the provisional codes
  and the refused-claims list.
- Tests: sign and verify under each mapped algorithm; each refused claim;
  each match kind mapped or refused, `nil` placement, and the moq-pattern
  vectors round-tripped both ways; expiry and not-before; the grant
  produced from every c4m-01 example; serve admitting a CAT over a real
  moq-transport session on every supported draft and refusing an unknown
  token kind, a bad MAC, and a token plus `jwt`; the CLI round trip.

Public API: `moq_auth::cat` new, `moq auth serve` and `moq auth sign|verify`
gain flags. Wire: none.

## Required

- [Setup token](/quest/m1/setup-token.md) - the token reaches the
  server's request
