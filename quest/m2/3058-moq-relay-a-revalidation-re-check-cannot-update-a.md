# [M] moq-relay: a revalidation re-check cannot update a session's tier, and changes its alias only by closing it

## Goal

Implement and verify the behavior tracked in [#3058](https://github.com/moq-dev/moq/issues/3058)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Found while reviewing #3041, which merged as 7047347. Not a regression - it is what the new re-check does and does not do with a reply whose *non-scope* fields changed.

`Auth::recheck` scores a reply by `Scope::covered_by`, which compares root, subscribe and publish. The resulting `AuthToken` is then dropped and only the `CacheHints` propagate:

```rust
Fetched::Ok { resp, hints } => match self.authorize(&grant.params, &resp) {
    Ok(token) if grant.scope.covered_by(&token) => Recheck::Valid { hints },
```

So two fields the auth API can legitimately change behave inconsistently:

**`tier` is silently ignored.** An endpoint that re-buckets a connection - moving it to a named billing tier, or off one - has no effect on a live session. The session keeps the tier it was admitted with until it reconnects. That matters because the tier decides which meter pays.

**`alias` closes the session.** The alias becomes the token's `root`, so `covered_by`'s `self.root == token.root` fails and the session is revoked. That is arguably correct - the broadcast would otherwise keep announcing under a root the API no longer assigns - but it is a silent hard disconnect for what may be a benign rename, and it is not obviously the intended contract.

The general question is what a re-check is allowed to *update in place* versus what forces a reconnect. Scope narrowing is settled (close, and the client reconnects into the narrower grant). `tier` and `alias` are not.

`tier` looks the harder half: the session's stats handle is built from the admission token in `connection.rs`, so applying a new tier mid-session means rebuilding that handle, and usage already recorded under the old tier stays there. Worth deciding deliberately rather than inheriting whichever behaviour fell out.

## Closes

- [#3058](https://github.com/moq-dev/moq/issues/3058) - close this issue when the quest finishes
