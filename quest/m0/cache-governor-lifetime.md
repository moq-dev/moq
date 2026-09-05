# [S] Cache governor stops with its owner

## Goal

A relay configured with cache headroom leaves no memory-sampling task behind
after setup fails or its last owning handle is dropped. Repeatedly loading and
dropping relays in one runtime does not accumulate governors. Keep the existing
capacity and headroom behavior while the relay is alive.

## Plan

`rs/moq-relay/src/cache.rs` starts `governor(pool.clone(), ...)` in
`CacheConfig::init` and discards the `JoinHandle`. The governor holds a strong
pool clone and loops forever. Neither `Cache` nor `Cluster` owns its shutdown.
`Relay::load` starts it before the fallible `Cluster::new` and client TLS build,
so both a later setup error and normal relay teardown strand the task. This is
visible in the ownership paths; add a runtime reproduction before fixing it.

- Remove the detached lifetime. Prefer an owned task guard or a driver that
  stops with its owner, using the existing cluster lifetime rather than a new
  process-wide task registry or shutdown callback.
- Keep the driver alive when an embedder moves `Relay::cluster` out of `Relay`
  or clones it for sessions. Dropping the temporary `Cache` during setup must
  not stop an otherwise live relay. Decide explicitly whether a separately
  retained pool also retains the governor.
- Cover setup failure after governor creation, normal last-owner drop, and
  dropping one of several cluster handles. Use task completion or an owned
  drop probe to prove shutdown, not just unchanged cache capacity. The
  last-owner regression must fail with the current detached spawn restored.
- Update `doc/bin/relay/config.md` if the ownership contract needs explanation
  for embedders. Avoid changing public construction shapes just to attach a
  guard; if a published API must break, target `dev`.

## Related

- [Construct the cluster origin once](/quest/m1/cluster-construction.md) -
  attaching cache settings should not replace an already exposed origin
