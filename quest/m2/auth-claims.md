# [M] Tokens are written as documented, with a typed jti

## Goal

A token minted by `moq-auth` or `@moq/auth` carries its grants as `publish`
and `subscribe`, the claims `doc/bin/relay/auth.md` documents, and never as
the 2024 `put` and `get` prefixes; both libraries still read `put` and `get`
from old tokens. `jti` is a typed, bounded string that a signer can set and
a verifier returns. `Algorithm` is an open enum. Claims and key sets are
read through the same names in Rust and JS. `KeyOperation` names only what
the libraries do.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ New tokens are written with `publish` and `subscribe` only, and old
  `put` and `get` are still read (rejected: writing both forever and
  documenting `put` and `get` as part of the format). The support rule is
  the newest and previous release, and `release` already reads the pattern
  claims.
- ✅ `jti` is a typed, bounded string that both libraries can mint and
  return (rejected: reading any JSON value and dropping it). Tightening it
  after the cut would refuse tokens accepted today.
- ✅ Claim and key set accessors have the same names in Rust and JS
  (rejected: leaving each language its own).
- ✅ `Algorithm` is `#[non_exhaustive]` (rejected: a break for the next
  algorithm).
- ✅ `KeyOperation::{Encrypt, Decrypt}` and JS `"encrypt"` and `"decrypt"`
  are removed, as part of the audit's rule that unused public API goes,
  once Kixel and moq.pro are checked for a caller.

Facts found by the audit:

- `Grants::encode` writes a subtree-only grant as `put` and `get`
  (`rs/moq-auth/src/wire.rs:224-250`; JS `encodeGrants`,
  `js/auth/src/wire.ts:166`). So `moq auth sign --publish 'alice/**'` writes
  `{"put":["alice"]}`, while the claims table documents only `root`,
  `publish`, and `subscribe` (`doc/bin/relay/auth.md:232-236`). A verifier
  written from the docs refuses the most common token this project mints.
- The pinned reason is moq-relay 0.14 with `moq-token` 0.7
  (`rs/moq-auth/tests/released_token.rs:3-9`). `release` ships moq-auth
  0.2.4, which reads `publish` and `subscribe`.
- `jti` parses as `Option<IgnoredAny>` (`wire.rs:54`) and
  `z.optional(z.unknown())` (`js/auth/src/claims.ts:82`), and is dropped.
  `Claims` is `#[non_exhaustive]`, so the field is additive in Rust. Token
  revocation, which moq.pro plans, needs an identifier.
- Names differ: Rust `Claims.{expires, issued, not_before}`
  (`rs/moq-auth/src/claims.rs:139-146`) against JS `exp`, `iat`, `nbf`
  (`claims.ts:69-73`), while JS `Grant` says `expires`; `Claims::authorize`
  and `Scope::allows` (`claims.rs:225,53`) against free `authorize()` and
  `scopeAllows()` (`claims.ts:160,203`); `KeySet::{to_public_set, find_key}`
  (`rs/moq-auth/src/set.rs:93,104`) against JS `KeySet.{public, find}`.
- `Algorithm` has no `#[non_exhaustive]` (`rs/moq-auth/src/algorithm.rs:10`),
  and its doc comment says "there's currently no point using public key
  crypto" (`:6`) while the docs recommend ES256.
- `KeyOperation::{Decrypt, Encrypt}` (`rs/moq-auth/src/key.rs:21-24`) and
  `OperationSchema`'s `"decrypt"` and `"encrypt"` (`js/auth/src/key.ts:25`)
  load and do nothing: `grep -rn 'KeyOperation::\(Encrypt\|Decrypt\)' rs`
  finds no use outside `key.rs`.

For the PR:

- Writing: delete the prefix branch of `Grants::encode` and its JS twin, and
  the helpers only it used (`Pattern::as_prefix` if nothing else calls it).
  Keep the readers and the mixed-encoding refusal. Rewrite
  `tests/released_token.rs` to pin what is still promised: an old `put` and
  `get` token verifies with the same scope, and a new token is the
  documented shape. Tokens minted by 0.22 stop verifying on moq-relay 0.14
  and earlier; say so in the Impact section.
- `jti`: pick the bound in the PR (the same in Rust and JS), refuse a `jti`
  that is not a string or is over it, and add both cases to the shared
  corpus. `moq auth sign` and the JS CLI take an optional value; neither
  invents one. `doc/bin/relay/auth.md:237` stops saying `jti` is ignored.
  Revocation itself is not in scope.
- Names: recommended to spell claims as the grant already does (`expires`,
  `issued`, `not_before` in Rust, `expires`, `issued`, `notBefore` in JS;
  the wire stays `exp`, `iat`, `nbf`), make `authorize` and `allows` methods
  or free functions in both, and use `KeySet::{public, find}` in both.
- `KeyOperation`: a key file whose `key_ops` lists `encrypt` or `decrypt` is
  refused after the removal, which is what strict key parsing already does
  for an unknown operation. Record it for `MIGRATE.md`. Keep the variants if
  the downstream check finds a key file that names them.
- Fix `Algorithm`'s doc comment while there.

Public API: breaking in `moq-auth` and `@moq/auth` (accessor names, the
`KeyOperation` variants, `Algorithm` made non-exhaustive); `jti` is
additive. Token format: new tokens no longer verify on moq-relay 0.14 and
earlier; a `jti` that is not a bounded string is refused. Wire: none.
