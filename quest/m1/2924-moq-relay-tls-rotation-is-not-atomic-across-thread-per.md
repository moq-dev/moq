# [M] One served identity shared by every listener

## Goal

Every listener the relay runs, tokio QUIC workers and io_uring workers alike,
resolves handshakes through one served-identity handle. A rotation replaces
that handle's snapshot once for the whole group, a watcher failure is one
failure, `/certificate.sha256` reads the same state every worker serves, and
`--listen-tls-generate` works with `runtime.workers` because the certificate
is generated once and shared. The io_uring workers gain the reload they lack
today (they read the certificate once at bind), which is what lets ACME
promise renewal on that runtime.

## Plan

Follow-up from #2921 (part 1 of #2875), which added `runtime.workers` and
documented the split rather than fixing it: each tokio worker builds its own
listener with `listen::Config::init`, so each loads the PEM files, spawns its
own `tls::reload_certs` watcher, and snapshots its own mTLS roots.
`Workers` keeps the first worker's `Certificates` handle for the fingerprint
endpoint. `uring::Workers::bind` reads one pair once and never reloads.

The primitive already exists: `ServeCerts` implements
`rustls::server::ResolvesServerCert` (`rs/moq-tokio/src/tls.rs:2857`) and
`reload_certs` (`:2931`) swaps its contents from the file watcher. What is
missing is sharing it.

- Build the `ServeCerts` and its watcher once, on the shared runtime, in
  `Relay::load`; hand every listener an `Arc` of it. A listener's
  `ServerConfig` is built around the shared resolver, so a swap is visible on
  the next handshake on every worker with no per-worker state.
- The mTLS client roots ride the same handle.
- `--listen-tls-generate` generates once into the shared handle; drop the
  #2921 refusal with workers.
- io_uring: `uring::Workers::bind` takes the shared resolver instead of a
  loaded pair. If the ring's TLS path cannot take a resolver, that is the
  finding to record and the refusal to keep, stated per backend rather than
  claimed.
- Atomicity is defined at the snapshot boundary: a handshake already holding
  the previous snapshot finishes with it; every resolution after a swap sees
  the new one.
- Tests: rotate the PEM pair under a multi-worker relay and assert every
  worker's next handshake and `/certificate.sha256` agree; a half-written
  pair keeps the old one serving; generate-with-workers serves one
  fingerprint. Run under the io_uring lane where the kernel allows.

Public API: additive on `moq-tokio` (a constructor taking the shared
resolver); `tls::Listen` is non-exhaustive, so no published signature
changes. Wire: none.

## Closes

- [#2924](https://github.com/moq-dev/moq/issues/2924) - close this issue when the quest finishes

## Related

- [Automatic ACME certificates](/quest/m1/709-automatic-letsencrypt-support.md) - feeds the shared resolver
