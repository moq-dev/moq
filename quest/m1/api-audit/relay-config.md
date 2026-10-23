# [M] Relay config

## Goal

moq-relay and every embedder load configuration through one call that
refuses old spellings, and relay config names do not collide with module
names or each other.

## Plan

Decided 2026-10-22 (API audit):

- One loader: `Config::merge_into(&CliLayer, &EnvLayer, Option<FileSource>)`
  takes about 25 lines of setup, so moq.pro's edge bypasses it
  (`toml::from_str` plus `update_from`) and `deprecated()` never runs:
  `MOQ_SERVER_BIND` or `[server]` are silently ignored there. Provide one
  call both binaries use (for example `moq_tokio::cli::load::<C>()`) with
  provenance, so an embedder can tell a set value from a default without
  comparing against `Default`.
- `[session]` and `[peer]` collide with `moq_relay::session` and
  `cluster::Peer`; rename to `[limits.client]` and `[limits.peer]`.
- `acme.dir` (a state path, documented required but typed `Option`) beside
  `acme.directory` (the URL): rename `dir` to `acme.cache`; rustls-acme moves
  behind an `acme` feature.
- `cluster.connect_api_tls_root` is flat while everything else nests
  `*.tls.root`: `[cluster.api] url`, `tls.root`.
- `auth.verifications`/`Auth::with_verifications(usize)` is used only by a
  bench and panics on 0: a private constant.
- `[listen|connect.extensions]` is TOML-only and unset everywhere: drop the
  config field and `Web::with_extensions`, keep the Rust API.
- `uring::Workers::spawn` then `serve()` is order-dependent (`serve()` alone
  no-ops): typestate `spawn(self) -> Spawned`.
- `cluster::with_client_tls` repeats TLS `with_connect` holds: delete.
- mTLS: `serve::Policy::{mtls_peer, mtls_upstream}` and the matching flags
  can express "upstream without peer", refused at runtime; take
  `Option<Link { Peer, Upstream }>` and one `--mtls-link` flag, keeping the
  `Grant` wire booleans.
- Public only for tests: `Cluster::admit`, `cluster::Admitted`, and
  `auth::hold` (move moq.pro onto them, since it keeps copies, or make them
  `pub(crate)`); `Incoming`/`Accepted` behind `test-support`.
- Extend the `demo_configs_parse` test to `packaging/` and `test/`.

Docs: `doc/bin/relay/`. Public API: moq-relay config and Rust API breaking;
old spellings are refused by name. Wire: none.
