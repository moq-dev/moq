# [S] An auth re-check moves the tier and names an alias change

## Goal

A revalidation reply that changes a session's `tier` takes effect on the live
session: later usage records under the new tier, what was already recorded stays
under the old one, and the session stays up. A reply that changes `alias` still
closes the session, but with its own expiry reason in the log and to the client
instead of a generic revocation, so a benign rename is distinguishable from a
refusal. Scope narrowing keeps the
[relay auth](/quest/m2/path-patterns/relay-auth.md) contract, and
`doc/bin/relay/auth.md` states the outcome per field.

## Plan

`Auth::recheck` in `rs/moq-relay/src/auth.rs` scores a reply with
`Scope::covered_by` (root, subscribe, publish), drops the `AuthToken`, and
propagates only `CacheHints`. The stats handle is built once at admission in
`connection.rs` from `token.tier` and `token.root`.

- `Recheck::Valid` carries the reply's `tier`. The connection compares it with
  the live handle's and, on change, rebuilds the session stats handle under the
  new tier and swaps it into the session's traffic accounting. Earlier counters
  are not migrated: the old tier was truthfully what paid until then.
- `Expired` gains an `Alias` variant, additive under `#[non_exhaustive]`, raised
  when the reply's alias no longer matches the admitted root (v0) or the
  canonical alias transform relay auth defines for v1 grants. `covered_by`
  keeps failing it; only the reason changes.
- Tests: a re-check that moves the tier records subsequent bytes under the new
  meter and leaves earlier bytes where they were; a re-check that changes the
  alias closes with `Expired::Alias`; a reply that changes both closes.
- Docs: the revalidation section of `doc/bin/relay/auth.md` gains a per-field
  table: scope narrowing resizes, tier updates in place, alias closes.

On main, additive.

## Closes

- [#3058](https://github.com/moq-dev/moq/issues/3058) - close this issue when the quest finishes

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - the scope contract this inherits
- [Auth verdict](/quest/m2/auth-verdict.md) - the proxy mode whose re-check this also governs
