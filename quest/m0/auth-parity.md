# [M] Auth parity

## Goal

A moq-relay 0.14.18 deployment either keeps working on 0.15 or is refused
with the fix named, never silently degraded or widened. Every credential a
session presents is either evaluated or refused: nothing is ignored, and no
two credentials are combined. A token that 0.14 accepted is still accepted,
unless it carries a claim outside the registered JWT set; that token is
refused with the claim named.

Non-goals: the redesign that #3688 documented stays. That covers
`--auth-key` moving to `moq auth serve`, the removal of `--auth-api`,
`--auth-domain`, and key URLs, and refusing an mTLS-only relay.

## Plan

Each fix lands with a regression test that fails without it.

### Startup

- **Refuse removed auth settings.** `[auth] key`, `key_dir`, `auth_api`,
  `domains`, `mtls_tier`, and `tls`, and the `MOQ_AUTH_KEY`, `_KEY_DIR`,
  `_API`, `_DOMAIN`, `_MTLS_TIER`, `_PUBLIC_API`, and `_TLS_*` env vars are
  ignored today, because `auth::Config` has no `deny_unknown_fields` and
  `Config::deprecated()` (`rs/moq-relay/src/config.rs`) skips `[auth]`. A 0.14
  config with `key` and `public` boots with JWTs ignored. Refuse each one at
  startup and name its replacement, as `doc/setup/upgrade.md` already
  promises. Also fix `demo/relay/prod.toml`, which still suggests `key_dir`.
- **No re-checks or expiry by default.** `moq auth serve` defaults
  `--expires` to 1 day and `--revalidate` to 1 minute, so anonymous sessions
  and tokens without `exp` drop daily. 0.14's static `key` and `public` never
  closed those; it did close at a token's `exp` and a certificate's
  notAfter, which stays. Both flags default to off. Refuse to start on:
  - `--revalidate` without `--expires`, since the contract refuses a cadence
    without a bound;
  - `--limit-*` without `--revalidate`, since the session table ages out
    slots by the cadence. Without it, limits either undercount or leak the
    slots of a relay that died.

  Document that without `--revalidate`, rotating or deleting a key does not
  close live sessions.

### Tokens

- **Registered claims only.** Tokens are wire. 0.14 ignored every unknown
  claim; today both languages refuse them. Ignoring them fails open, because
  `root` defaults to `""`: an issuer's typo such as
  `{"rooot":"room/123","publish":["**"]}` would grant the whole relay, and so
  would a future narrowing claim read by an older verifier. So:
  - `iss`, `sub`, and `jti` are accepted and ignored. `iat` is read and not
    enforced, as today.
  - `nbf` is enforced, like `exp`. JS (`jose`) enforces it today; Rust
    ignores it.
  - `aud` is refused. 0.14 refused it (jsonwebtoken's default
    `validate_aud`), and so does Rust today, but JS accepts it. No audience
    is configured, so there is nothing to check it against.
  - Any other claim is refused with its name in the error, including
    `cluster`, which 0.14 accepted and ignored. List this in the upgrade notes:
    an issuer adding app claims such as `user_id` must drop them.

  Both languages must agree on every case above; add them to the shared
  vectors.
- **0.14 verifies what we sign.** Signing already writes subtree-only grants
  as legacy `put`/`get` prefixes (`foo/**` as `foo`, `**` as `""`), and
  anything else as `publish`/`subscribe`. 0.14 reads the latter as granting
  nothing and refuses the token. The field names are the version, so nothing
  new is needed on the wire. Pin it with a test that verifies tokens from
  `moq auth sign` and `@moq/auth` against the published `moq-token` 0.7.
  Subtree grants must be accepted with the same scope, and exact grants
  refused.

### Credentials

Today a session can present a JWT, a verified certificate, or both, and the
answer depends on which mode the relay runs in. Make it one rule: a relay
evaluates the credentials it is configured for, and refuses anything else.

| Presented | `--auth-public` | `--auth-url` to `moq auth serve` |
|---|---|---|
| nothing | public rules | `--public-*`, or refused |
| JWT | refused | verified, or refused |
| certificate | never requested (see below) | `--mtls-*`, or refused |
| JWT and certificate | never requested | refused |

- **Refuse a JWT on a public-only relay.** Under `--auth-public`, a client that
  sends `?jwt=` is admitted on the public grant. 0.14 answered 401
  `UnexpectedToken`. Refuse every form: the query, a SETUP token of any
  type, and the HTTP endpoints. Peers that send `cluster.token` to a
  public-only relay are refused too, so say so in the upgrade notes.
- **Refuse a client CA on a public-only relay.** Under `--auth-public`, a
  certificate grants the public grant, so `listen.tls.root` and
  `web.https.root` only make browsers offer certificates for nothing. Refuse
  them at startup, pointing at `moq auth serve --mtls-*`. Watch for the
  rolling-upgrade exemption in `connection::authorize`, which shows hidden
  routes to any verified certificate. It widens nothing beyond the grant, but
  check that no documented mesh depends on it before refusing.
- **Refuse a JWT and a certificate together** in `moq auth serve`, like
  `TwoTokens`. Today the JWT wins, so a mesh that follows the migration docs
  (`--mtls-*` without `--key`) refuses peers that also send `cluster.token`
  with `NoKeys`. 0.14 checked the certificate first. Neither order is safe:
  certificate-first lets any certificate from the client CA override a JWT
  meant to narrow it, and JWT-first refuses or narrows a peer by accident. A
  refusal that names both makes the operator drop one. The migration docs
  must say that `cluster.token` and a peer certificate are alternatives.

Declined: failing a client whose configured certificate was never requested.
One `connect.tls` identity serves both the auth server and peers, and a
dialer cannot know which credential the server means to check.

### Declined

Each of these was considered and kept as 0.15 behavior:

- A 0.14 peer that dials with a cluster JWT no longer sees
  `.internal/origins` gossip (#4060). Peers identify by certificate or LAN
  path.
- An alias `grant.root` of any depth is accepted. 0.14 required the dialed
  path's depth, which hard-codes one aliasing scheme; a server that moves the
  root owns the scope it hands out.
- A path in `--cluster-connect` is not refused, although it shifts the mesh
  frame.
- `moq auth serve --key` takes a file path only, not 0.14's https or JWKS URL.
- `moq auth sign --root` (token root) and JS `verify --root` (dialed path)
  keep their names.
- `/.cluster*` roots are reserved.
- `--auth-public a,b` splits on commas.

### Docs

- Put the credentials table in `doc/bin/relay/auth.md`, and replace the
  "Policy runs in this order" list with it.
- Record each restored or refused behavior in `doc/bin/relay/auth.md` and
  `doc/setup/upgrade.md`. List the declined differences in the upgrade notes,
  since none of them is documented today.

## Required

- [Public rules parity](/quest/m0/public-rules.md) - same auth code; one owner at a time

## Related

- [Release](/quest/m0/release.md) - ships this parity
