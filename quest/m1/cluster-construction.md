# [M] Construct the cluster origin once

## Goal

A cluster exposes one stable origin from construction onward. Applying cache
settings cannot silently detach previously derived origin handles or stats
publishers. Callers do not need to know the order of origin-rebuilding methods.

## Plan

`rs/moq-relay/src/cluster.rs` constructs an origin in `Cluster::new` (:661),
exposes it through the public `origin` field, then constructs another in
`with_cache` (:700-709). It retains `info` alongside the live origin to
support that replacement and rebinds `nodes` afterward. A caller can clone
`cluster.origin` or attach stats before calling `with_cache`; those handles
keep the old origin. Consuming `self` in the builder does not prevent this
because the origin is cloneable. The two call sites, `Relay::load`
(rs/moq-relay/src/relay.rs:217) and the cache test helper
(rs/moq-relay/src/cache.rs:269), order the calls correctly, but the public API
only documents the prerequisite that the origin must still be pristine.

- Put origin-defining settings into construction, using one options struct
  with defaults. Construct the origin and its node view once, after the cache
  pool, retention ceiling, and identity are known. `linger` is not one of
  them: it is a deprecated, hidden no-op that only logs a warning
  (cluster.rs:485-494, :670-675), so drop it rather than carry it into the
  options.
- Delete the origin-rebuilding `with_cache` path and any stored construction
  state that has no remaining purpose. Keep builders that only attach
  independent services if they do not invalidate existing handles.
- Migrate both call sites and audit external embedder usage before removing
  the published method. This is a `dev` change. Do not add a compatibility
  shim that retains the same origin replacement hazard.
- Update the docs that name `with_cache`: the governor paragraph in
  doc/bin/relay/config.md:193 and the `Cache` doc comment at cache.rs:74.
- Verify that configured cache settings reach the same origin used by serving,
  node discovery, and stats. Cover the API shape at compile time where possible
  and exercise publish/consume through a retained origin handle.
