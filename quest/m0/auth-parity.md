# [M] Auth parity

## Goal

The relay auth behaviors that 0.15 changed without documenting it behave as
they did in moq-relay 0.14.18. A 0.14 deployment either keeps working or is
refused at startup with its replacement named, never silently degraded. A
token that 0.14 accepted is still accepted.

Non-goals: the redesign that #3688 documented stays. That covers
`--auth-key` moving to `moq auth serve`, the removal of `--auth-api`,
`--auth-domain`, and key URLs, and refusing an mTLS-only relay.

## Plan

Each fix lands with a regression test that fails without it.

### Restore

- **Refuse removed auth settings.** `[auth] key`, `key_dir`, `auth_api`,
  `domains`, `mtls_tier`, and `tls`, and the `MOQ_AUTH_KEY`, `_KEY_DIR`,
  `_API`, `_DOMAIN`, `_MTLS_TIER`, `_PUBLIC_API`, and `_TLS_*` env vars are
  ignored today, because `auth::Config` has no `deny_unknown_fields` and
  `Config::deprecated()` (`rs/moq-relay/src/config.rs`) skips `[auth]`. A 0.14
  config with `key` and `public` boots with JWTs ignored. Refuse each one at
  startup and name its replacement, as `doc/setup/upgrade.md:14-16` already
  promises. Also fix `demo/relay/prod.toml:41-42`, which still suggests
  `key_dir`.
- **Accept unknown JWT claims.** Tokens are wire: `rs/moq-auth/src/wire.rs`
  and `js/auth/src/claims.ts` (`z.strictObject`) refuse any unknown claim,
  including `sub`, `iss`, `aud`, `nbf`, and `jti`, which 0.14 ignored (#3684).
  Ignore them in both languages and flip the test that asserts refusal.
  `cluster: true` is the exception: refuse it with a message, since it
  never granted peer status even in 0.14. Refusing a malformed JWK at load
  (an `oct` key without `kty`) is fine, since it fails at startup.
- **No re-checks or expiry by default.** `moq auth serve` defaults
  `--expires` to 1 day and `--revalidate` to 1 minute, so anonymous sessions
  and tokens without `exp` drop daily. 0.14 never re-checked or closed them.
  Both default to off. `--revalidate` without `--expires` refuses to start,
  because the contract refuses a cadence without a bound. A grant with no
  bound of its own then carries neither.
- **Refuse a JWT on a public-only relay.** Under `--auth-public` with no
  `--auth-url`, a client that sends `?jwt=` is admitted on the public grant.
  0.14 answered 401 `UnexpectedToken`. Refuse it.
- **mTLS before JWT in `moq auth serve`.** 0.14 checked the certificate first.
  `serve::Policy::decide` checks the JWT first, so a mesh that follows the
  migration docs (`--mtls-*` without `--key`) refuses peers that also send
  `cluster.token` or `?jwt=`. Check `--mtls-*` first when a certificate is
  present and the mTLS rules are set.

### Declined

Each of these was considered and kept as 0.15 behavior:

- A certificate on an `--auth-public` relay gets the public grant, not 0.14's
  unrestricted access. Meshes use `moq auth serve --mtls-*`.
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

Record each restored behavior in `doc/bin/relay/auth.md` and
`doc/setup/upgrade.md`. List the declined differences in the upgrade notes,
since none of them is documented today.

## Required

- [Public rules parity](/quest/m0/public-rules.md) - same auth code; one owner at a time

## Related

- [Release](/quest/m0/release.md) - ships this parity
