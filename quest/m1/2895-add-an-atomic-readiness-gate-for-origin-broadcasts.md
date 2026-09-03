# [S] Add an atomic readiness gate for Origin broadcasts

## Goal

Implement and verify the behavior tracked in [#2895](https://github.com/moq-dev/moq/issues/2895)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### After prefix routes

Half of the stated problem is gone.
[moq#3225](https://github.com/moq-dev/moq/pull/3225) made
`origin::Producer::create_broadcast` stop announcing: the blessed order is
create, populate, announce, and `announce` is a separate call returning a guard.
So a broadcast is no longer announced before its tracks exist, and
`broadcast::Route::announce = false` (which the issue correctly says is not a
readiness gate) does not exist either.

What remains is the exact-path half: `create_broadcast` still makes the source
visible to `request_broadcast` immediately, so a concurrent consumer can find a
broadcast whose tracks and `broadcast::Dynamic` handler are not installed yet.
Re-scope to that, and weigh it against the convention now being sufficient for
the announced path.

### Issue context

#### Problem

`origin::Producer::create_broadcast` makes an eligible source visible by exact-path lookup, and may announce it, before the caller has installed all of its tracks or a `broadcast::Dynamic` handler. A concurrent consumer can therefore find the broadcast during a partially prepared state.

`broadcast::Route::announce = false` is not a readiness gate. It suppresses announcement events, but the broadcast remains reachable through exact-path lookup.

#### Goal

Add an atomic readiness mechanism so callers can prepare a broadcast completely before it becomes discoverable by either announcements or exact-path lookup.

The API should make the safe publication sequence clear and difficult to misuse. Decide whether readiness belongs in construction, a consuming terminal operation, or a small typestate/builder boundary before implementation.

#### Scope

- Define one atomic transition from hidden/preparing to visible.
- Gate both exact-path lookup and announcements on that transition.
- Cover local sources, replacements, and sources initially parked behind an incumbent.
- Specify what dropping an uncommitted source does.
- Add race-focused tests proving consumers never observe partial readiness.

#### Relationship

Related to #1073, but does not block it. #1073 deliberately preserves the current immediate-visibility behavior while making Origin lifecycle caller-driven.

## Closes

- [#2895](https://github.com/moq-dev/moq/issues/2895) - close this issue when the quest finishes

