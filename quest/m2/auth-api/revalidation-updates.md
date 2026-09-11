# [M] An auth re-check moves the tier, names an alias change, and says when it is off

## Goal

A revalidation reply that changes a session's `tier` takes effect on the live
session: later usage records under the new tier, what was already recorded stays
under the old one, and the session stays up. A reply that changes `alias` still
closes the session, but with its own expiry reason in the log and to the client
instead of a generic revocation, so a benign rename is distinguishable from a
refusal. Scope narrowing keeps the
[relay auth](/quest/m2/path-patterns/relay-auth.md) contract. An endpoint
reply that schedules no re-check is logged once, so a relay whose revalidation
is off no longer looks like one whose endpoint has never revoked anything, and
`doc/bin/relay/auth.md` states the outcome per field and the revocation window
an operator can actually get.

## Plan

Today `Auth::recheck` in `rs/moq-relay/src/auth.rs` scores a reply with
`Scope::covered_by` (root, subscribe, publish), drops the `AuthToken`, and
propagates only `CacheHints`; the revalidation loop consumes `Recheck::Valid`
to reschedule itself and nothing reaches the connection. The stats handle is
built once at admission in `connection.rs` from `token.tier` and `token.root`,
handed into the request before acceptance, and cloned into the origins, scopes
and meters, so rebuilding a local handle would retag nothing.

- `Recheck::Valid` carries the reply's `tier` beside the hints, never the whole
  `AuthToken`. The revalidation loop hands it to the shared `stats::Session`
  state, which holds the current tier behind an in-place swap and treats an
  unchanged tier as a no-op, so `A -> B -> B` retags once and `A -> B -> C`
  twice, and every clone the origins and meters hold records subsequent bytes
  under the current tier. Earlier counters are not migrated: the old tier was
  truthfully what paid until then. This is the substantive piece; size it
  before the alias half.
- `Auth::recheck` compares the alias explicitly before scoring coverage, and a
  changed alias becomes its own `Recheck` outcome rather than falling through
  `covered_by` into `Recheck::Revoked`. The loop maps it to a new
  `Expired::Alias` variant, additive under `#[non_exhaustive]`; scope loss and
  refusals keep `Expired::Revoked`. Under v0 the comparison is against the
  admitted root; under v1 it is the canonical alias transform relay auth
  defines. Alias takes precedence when both alias and tier change.
- Tests: a re-check that moves the tier records subsequent bytes under the new
  meter through an already-cloned handle and leaves earlier bytes where they
  were; a re-check that changes the alias closes with `Expired::Alias`; a reply
  that changes both closes.
- A reply that schedules no re-check (no `max-age`, `max-age=0`, `no-cache`,
  `no-store`, or a value the relay cannot parse) is the endpoint opting out,
  and stays so. The relay says it once: the first such reply from an endpoint
  logs at WARN that sessions admitted under it are never re-checked and end
  only at their credential's `exp`. One line per relay, never per session.
- Docs: the revalidation section of `doc/bin/relay/auth.md` gains a per-field
  table: tier updates in place, alias closes, and scope narrowing closes today
  (`covered_by` failing maps to `Recheck::Revoked`) and resizes once
  [relay auth](/quest/m2/path-patterns/relay-auth.md) lands; say which is in
  effect. The same section and the `--auth-api` flag help state the cadence
  contract where an operator sizes a revocation SLA: `max-age` arms
  re-checks, floored at one second; the values above disable them; and
  because re-checks ride the admission cache, revocation takes up to twice
  `max-age`. The `revalidate` doc comment in `rs/moq-relay/src/auth.rs` on
  main repeats one paragraph and claims a 3x staleness window the code does
  not have; dev already rewrote it, so take dev's wording.

On main, additive.

## Closes

- [#3058](https://github.com/moq-dev/moq/issues/3058) - close this issue when the quest finishes
- [#3605](https://github.com/moq-dev/moq/issues/3605) - close this issue when the quest finishes

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - the scope contract this inherits
