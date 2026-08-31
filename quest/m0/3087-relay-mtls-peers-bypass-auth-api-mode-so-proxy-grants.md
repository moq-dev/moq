# [M] relay: mTLS peers bypass --auth-api-mode, so proxy grants can't refuse or scope them

## Goal

Implement and verify the behavior tracked in [#3087](https://github.com/moq-dev/moq/issues/3087)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`--auth-api-mode proxy` (#3044) lets an auth endpoint decide every connection - except mTLS ones, which bypass the mode entirely.

`Auth::verify_mtls` calls `resolve_mtls`, which builds its own request and reads only `alias` and `tier` off the reply. A `200 {}`, or a reply carrying a deliberately narrow `grant`, still becomes an unrestricted publish-and-subscribe token. So an operator delegating authorization to their endpoint cannot refuse or scope an mTLS client through the documented grant response. `resolve_mtls` also hardcodes `host: None`, so host-routed tenants dialing the same path produce indistinguishable lookups.

#### Shape

Stop special-casing mTLS in the authorization path. It should build its request through `api_request` (sending `mtls=true`, and `host` in proxy mode) and resolve through `authorize`, like any other connection:

- **token mode**: `mtls=true` satisfies "has a credential" without a JWT or `key` - the cert *is* the token. Absent a grant the peer stays unrestricted, exactly as today, so existing deployments returning `{alias, tier}` are unaffected.
- **proxy mode**: the endpoint returns a `grant` like anyone else, and no grant is a refusal - consistent with the rest of the mode.

#### Deliberately NOT in scope: revalidation

mTLS peers keep `revalidate: None`. Not because mTLS is precious, but because a deployed endpoint sending a blanket `Cache-Control: max-age` on every reply would silently arm revalidation on a production relay mesh the moment the relay ships - gating fleet interconnect on that endpoint staying reachable, with no one having chosen it.

If mesh revalidation is wanted later it should be its own change, with the endpoint opting in deliberately for `mtls=true`, and a **relay-side floor** on the staleness window for those requests so an operator who forgets the header doesn't get a fleet that partitions on the first auth blip. Note `stale-if-error` alone is not sufficient protection: it only applies when the endpoint *errors*, so an endpoint that successfully answers "no" still partitions the mesh instantly.

## Closes

- [#3087](https://github.com/moq-dev/moq/issues/3087) - close this issue when the quest finishes
