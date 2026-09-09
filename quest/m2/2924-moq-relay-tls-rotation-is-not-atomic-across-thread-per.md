# [XL] moq-relay: TLS rotation is not atomic across thread-per-core QUIC workers

## Goal

Every tokio QUIC worker serves the same certificate at the same moment, a
rotation applies to the whole group at once, `/certificate.sha256` is
authoritative for every worker, and `--listen-tls-generate` works with
`runtime.workers`.

## Plan

Follow-up from #2921 (M1 part 1 of #2875), which added `runtime.workers` and
documented this rather than fixing it.

### Mechanism

Each tokio QUIC worker builds its own listener with `listen::Config::init`,
so each independently:

- loads the `listen.tls.cert` / `listen.tls.key` files,
- spawns its own `tls::reload_certs` watcher,
- snapshots its own mTLS client roots.

`Workers` keeps the *first* worker's `Certificates` handle, and that is the
one `/certificate.sha256` publishes.

Three consequences:

1. **Rotation is not atomic.** During a reload, workers can be serving
   different certificates. Both are valid, so TLS still completes, but the
   group is briefly inconsistent and the published fingerprint may match only
   some of them.
2. **A failed watcher diverges permanently.** `tls::reload_certs` logs and
   continues when it cannot watch; that worker then serves the old
   certificate indefinitely while its siblings rotate, and nothing surfaces
   the split.
3. **N redundant watchers** on the same files, one per worker.

The mTLS roots have the same shape: `--listen-tls-root` is snapshotted per
worker.

#2921 rejects `--listen-tls-generate` with workers, since that case is not
merely inconsistent but broken: each worker would generate a *different*
self-signed certificate while the fingerprint endpoint advertises one of
them.

### What exists

`tls::Listen::identity: Option<Identity>` (rs/moq-tokio/src/tls.rs:1240) is
the in-memory served identity, but it is not the handle this needs:

- Its only constructor is `Identity::generate` (:294). Nothing builds one
  from on-disk PEM, so the relay cannot load once and hand the result to N
  listeners.
- It is static. An `Identity` has no reload; the watcher only follows
  `cert`/`key` paths.
- It is additive. `ServeCerts::load_certs` (tls.rs:2805) pushes it onto
  the same list as the `cert`/`key` files and the `generate` hostnames
  (:2832-2834), so it is served *alongside* disk material, not instead of it.

The io_uring path is the prior art: `uring::Workers::bind` reads exactly one
certificate/key pair once, on the shared runtime, and hands every worker the
same material (rs/moq-relay/src/uring.rs:118-120). It refuses `tls.generate`
for the same reason the tokio group does (:126-128), and it does not reload.

### Direction

Give `moq-tokio` one served-identity handle that is loadable from PEM or
generated, hot-reloadable, and shared by reference: the relay loads and
watches once on the shared runtime and every listener (tokio workers and
io_uring workers alike) resolves certificates through the same handle.
Rotations then apply to the group at once, a watcher failure is one failure,
and `--listen-tls-generate` with workers is "generate once, share it". The
mTLS roots ride the same handle.

Sized XL because it reshapes `tls::Listen` (a published `moq-tokio` API used
by every binary), touches both worker runtimes, and needs a rotation test
that proves every worker flips in one step.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#2924](https://github.com/moq-dev/moq/issues/2924) - close this issue when the quest finishes
