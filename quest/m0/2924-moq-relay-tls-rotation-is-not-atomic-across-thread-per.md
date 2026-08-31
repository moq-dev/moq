# [XL] moq-relay: TLS rotation is not atomic across thread-per-core QUIC workers

## Goal

Implement and verify the behavior tracked in [#2924](https://github.com/moq-dev/moq/issues/2924)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up from #2921 (M1 part 1 of #2875), which added `runtime.workers`. Documented there rather than fixed, because the fix needs an API `moq-tokio` does not have yet.

#### Mechanism

Each QUIC worker builds its own listener with `listen::Config::init`, so each independently:

- loads the `listen.tls.cert` / `listen.tls.key` files,
- spawns its own `tls::reload_certs` watcher,
- snapshots its own mTLS client roots.

`Workers` keeps the *first* worker's `Certificates` handle, and that is the one `/certificate.sha256` publishes.

Three consequences:

1. **Rotation is not atomic.** During a reload, workers can be serving different certificates. Both are valid, so TLS still completes, but the group is briefly inconsistent and the published fingerprint may match only some of them.
2. **A failed watcher diverges permanently.** `tls::reload_certs` logs and continues when it cannot watch; that worker then serves the old certificate indefinitely while its siblings rotate, and nothing surfaces the split.
3. **N redundant watchers** on the same files, one per worker.

The mTLS roots have the same shape: `--listen-tls-root` is snapshotted per worker.

#### Why it was not fixed in #2921

Injecting one shared, already-loaded identity into every worker needs `moq-tokio` to accept in-memory certificate material. `tls::Listen` takes paths (`cert: Vec<PathBuf>`, `key`, `root`) and each backend loads them itself, so there is no way to hand N listeners one resolved, hot-reloadable identity today.

\#2921 documents the divergence window instead of implying it is not there, and separately rejects `--listen-tls-generate` with workers, since that case is not merely inconsistent but actively broken (each worker would generate a *different* self-signed certificate while the fingerprint endpoint advertises one of them).

#### Suggested direction

Give `moq-tokio` a way to build a listener from resolved TLS material rather than paths, then have the relay load and watch once on the shared runtime and hand every worker the same handle. Rotations then apply to the group at once and `/certificate.sha256` is authoritative for every worker. That would also let `--listen-tls-generate` work with workers: generate once, share it.

Reported by an adversarial review pass (Codex) on #2921.

## Closes

- [#2924](https://github.com/moq-dev/moq/issues/2924) - close this issue when the quest finishes
