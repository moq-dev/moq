# [L] JWT-free verdict mode

## Goal

moq-relay can hand an opaque credential to its auth API and be told the grant,
instead of resolving a `kid` and verifying a JWT locally.

## Plan

- Today the relay's auth-API request carries the connection path, the `kid`, an
  mTLS flag, and the transport, and never the credential itself. Nothing
  downstream of the relay can decide anything a local signature check could
  not, so the protocol change is the whole unit of work. It is what blocks an
  operator's auth endpoint from forwarding authorization to its own customers
  (moq.pro's downstream bring-your-own-auth Worker builds on this, and stays
  there).
- Carry the credential as `Authorization: Bearer <credential>` on the existing
  GET, and have the endpoint answer with `Vary: Authorization`. That keeps the
  response cacheable per credential while keeping a bearer secret out of URLs and
  access logs. Fall back to a request body only if the relay's cache middleware
  mishandles `Vary`; either way, record which and why in the PR.
- Send the credential for a VERDICT lookup only, and never on key resolution.
  Varying a key response on the credential would split one `kid` into a cache
  variant per JWT, so a shared-key audience that costs one request per relay per
  cadence today would cost one per viewer - collapsing the property that makes the
  JWT path the scale path, and for nothing, since the key a `kid` resolves to
  does not depend on which token presented it. Key responses stay keyed on
  `kid`, path, and transport.
- The response returns the grant directly (publish and subscribe scopes plus an
  expiry) beside the alias and tier it already returns, instead of a key. Keep
  ONE endpoint and ONE response type: verdict mode is a response shape, not a
  second flag, so the same endpoint can answer with a key for one connection and
  a grant for another and an operator migrates per connection rather than per
  deployment.
- Reconcile with revalidation in the same change. The re-check decides a grant is
  gone by the absence of a key, and a verdict grant has no key, so verdict-mode
  sessions would close on their first re-check. "Still vouched for" has to become
  "the response still carries a grant or a key" before either feature is correct
  with the other enabled.
- There is no JWT to read an `exp` from, so the response's expiry becomes the
  outer bound, and its absence leaves the revalidation cadence as the only bound.
  Say so where the endpoint contract is documented: an endpoint that returns
  neither is asking for a session that ends only when the API says so.
- The caching trade-off belongs in `doc/bin/relay/auth.md` next to the contract,
  not in a design note: a credential shared across an audience caches like a
  `kid` and costs one request per relay per cadence, while a per-viewer credential
  costs a request per viewer. That sentence lets an operator choose deliberately
  instead of discovering the bill.
- Test coverage mirrors the existing auth-API suite: a granted verdict, a
  refused one, a malformed body, the cache behavior under `Vary`, and a
  verdict-mode session surviving a re-check.
