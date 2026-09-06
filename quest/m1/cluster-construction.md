# [M] Construct the cluster origin once

## Goal

A cluster exposes one stable origin from construction onward. Applying cache
settings cannot silently detach previously derived origin handles or stats
publishers. Callers do not need to know the order of origin-rebuilding methods.

## Plan

`rs/moq-relay/src/cluster.rs` constructs an origin in `Cluster::new`, exposes
it through the public `origin` field, then constructs another in `with_cache`.
It retains `info` alongside the live origin to support that replacement and
rebinds `nodes` afterward. A caller can clone `cluster.origin` or attach stats
before calling `with_cache`; those handles keep the old origin. Consuming
`self` in the builder does not prevent this because the origin is cloneable.
`Relay::load` orders these calls correctly, but the public API only documents
the prerequisite that the origin must still be pristine.

- Put origin-defining settings into construction, using one options struct
  with defaults. Construct the origin and its node view once, after the cache
  pool, retention ceiling, identity, and linger are known.
- Delete the origin-rebuilding `with_cache` path and any stored construction
  state that has no remaining purpose. Keep builders that only attach
  independent services if they do not invalidate existing handles.
- Migrate `Relay::load` and audit external embedder usage before removing the
  published method. This is a `dev` change. Do not add a compatibility shim
  that retains the same origin replacement hazard.
- Verify that configured cache settings reach the same origin used by serving,
  node discovery, and stats. Cover the API shape at compile time where possible
  and exercise publish/consume through a retained origin handle.

## Related

- [Cache governor lifetime](/quest/m0/cache-governor-lifetime.md) - the
  headroom task needs an owner that survives moving or cloning the cluster
