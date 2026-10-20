# [M] Spike: relay per thread, clustered

## Goal

Measure a relay per worker thread, each with its own `Cluster` and origin and
nothing shared, clustered over the
in-memory transport (`moq_net::transport::memory`) instead
of QUIC. Compare it with the
[replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md), which
shares the control plane. The spike is a draft PR that never merges; its
numbers and a recommendation go in the
[epic](/quest/m2/perf/kio-channel-contention/README.md).

## Plan

- Cluster links dial only by URL over QUIC today
  (`run_remote_session`, `rs/moq-relay/src/cluster.rs`); the spike adds a
  throwaway seam to hand a cluster a pre-made memory session.
  `moq_net::Client::connect` and `Server::accept` already take a `memory::Session`,
  which meets their `Boxable` and `MaybeSync` bounds.
- Everything crosses workers through moq-lite: announcements, routes, and
  groups. That isolates what sharing the control plane saves, and exercises
  the cluster path the relay must keep working anyway.
- Flow control throttles only the hop, as in the replica spike.
- Same measurements and win criteria as the replica spike, plus announce propagation delay
  between workers and the memory of W announce tables.
- A win goes to `/quest-plan` for a real implementation quest; a no-win
  restores the perf epic's no-sharding rule.
