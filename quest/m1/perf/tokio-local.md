# [XS] Local spawn on moq-tokio's pinned workers

## Goal

moq-tokio builds one current-thread tokio runtime per pinned worker, but its
`Spawner::run` and its `moq_net::Runtime` impl still require `Send + 'static`
futures and spawn through the work-stealing-shaped API. moq-net's lite path
deliberately carries no `Send` bounds anymore (the runtime module documents
it, and a `!Send` compile-test transport proves it), and the io_uring workers
already spawn local futures. The bound on moq-tokio is an API artifact.
Relax the pinned-worker spawn path to accept `!Send` futures.

## Plan

- Spawn onto the worker's own current-thread runtime with a local mechanism
  (`LocalSet`/`spawn_local` or an equivalent local task set), keeping the
  session pinned to its worker exactly as today.
- The shared multi-thread tokio runtime (origin driver, auth, supervision)
  keeps its `Send` bounds; only the per-worker path relaxes.
- No behavior change and no expected bench movement; the win is that future
  worker-local state on the tokio backend stops needing `Arc`/`Mutex` just
  to satisfy a bound the runtime never exercises. Validate with
  `just bench BASE` neutrality and the existing runtime tests.
