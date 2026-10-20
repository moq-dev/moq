# [M] Spike: per-worker replicas

## Goal

Measure whether per-worker data-plane replicas cut lock wait on the
thread-per-core relay (`--runtime-workers`). Each worker serves tracks and
groups from its own origin, fed once per worker over the
in-memory transport (`moq_net::transport::memory`), while
announcements and routes stay in one shared origin. The spike is a draft PR
that never merges; its numbers and a recommendation go in the
[epic](/quest/m2/perf/kio-channel-contention/README.md).

## Plan

- Today every worker's sessions share one `origin::Producer`, built in
  `Cluster::new` (`rs/moq-relay/src/cluster.rs`) and cloned per worker
  (`rs/moq-relay/src/relay.rs`), so a subscriber on worker B reads state
  written on worker A.
- With replicas, a track subscribed on worker B is subscribed once over a
  memory session to the worker that holds it, then fanned out locally. Every
  consumer of a replica is on one thread, so its kio mutex is uncontended, and
  cross-core readers per frame drop from the subscriber count to the worker
  count.
- Control plane shared, data plane replicated: the shared origin still
  carries announcements, so announce churn is not paid once per worker.
- Flow control throttles only the hop (decided 2026-10-14): a slow worker's
  stream lags and its stale groups reset; the publisher and other workers
  never wait.
- Measure against the [epic](/quest/m2/perf/kio-channel-contention/README.md)'s
  baseline: `just bench-profile lock` fanout and chat at 1, 4, and 16 workers, plus
  relay CPU, resident memory (replicated group state), and glass-to-glass
  latency added by the hop. Lower lock wait alone is not a win; weigh it
  against the CPU, memory, and latency. Chat (`bench/workloads/chat.toml`)
  is one broadcast to 2,000 viewers with one-frame groups, so it stresses
  per-group track writes rather than frame fan-out. Replicas give little to
  low-fanout load; if that matters, name a separate low-fanout workload
  instead of reading it from chat.
- A win goes to `/quest-plan` for a real implementation quest; a no-win
  restores the perf epic's no-sharding rule.
