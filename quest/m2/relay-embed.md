# [M] An embedder reaches what the relay binary has

## Goal

A program that embeds `moq_relay` gets the same pieces its own `main` uses
without cloning config fields before `Relay::load` eats them, re-implementing
the CLI-plus-TOML merge, or building test relays from TOML strings. moq.pro's
edge is the reference: `rs/edge/src/{config.rs, main.rs, ops.rs}` and
`rs/transcode/tests/support/relay.rs` carry the copies.

## Plan

All additive on `moq-relay` and `moq-tokio`, so on main after the merge:

- `Relay::config(&self) -> &Config` (the resolved one) and
  `cluster::Cluster::id()`; the edge clones `listen.tls`, `cluster.id`, and
  four stats fields before `load` and keeps a field-for-field copy of
  `stats::Config`.
- `Config::parse_and_merge` and the `settings` registry become public
  (`moq_relay::settings()` plus a `merge_into` that composes an embedder's
  registry), so the edge's `config.rs` merge and its six clone-and-restore
  fields go. This waits on the `cli::Merge` shape from
  [moq-tokio shapes](/quest/m1/api-tokio-shapes.md).
- `auth::Config::public_grant()` and `is_empty()` are public; the edge
  `mem::take`s the two pattern lists to rebuild the union.
- `Relay::with_listeners(self, impl IntoIterator<Item = accept::Health>)` so
  an embedder-owned listener's accept health reaches the relay's
  `/metrics`; `Internal::with_listeners` exists but `load` consumes the
  `Internal` first. Consider `accept::Liveness` for a UDP-multiplexed
  listener that can only die wholesale.
- `tls::Listen::server_config_reloading(alpn)` backed by the existing file
  watchers, so an embedder's RTMPS listener reloads like `:443` instead of
  snapshotting a `ServerConfig` and re-implementing SIGHUP.
- `DEFAULT_MAX_STREAMS` lives in moq-tokio and the client default uses it;
  the edge hand-copies it. Custom TLS roots keep replacing the system store
  by default (a private CA is supplied to restrict trust); the edge opts
  into both with `system_roots`, which already exists.
- `Relay::web_addrs()` and `Relay::ready()`, and a `test-support` feature
  with `test_relay() -> TestRelay { relay, quic, http, url }` on ephemeral
  ports with a generated certificate; both downstream fixtures race for free
  ports and poll `/certificate.sha256`, and both use `[server]`/`[web.http]`
  spellings the merged relay refuses.
- CORS as an outer layer applied in `Relay::run` after `with_web`, or a
  `Web::cors()` the embedder reuses; merged routers lose the layer today.
- `Relay::quic_addr() -> Result<SocketAddr>` with the relay's own message
  where `addr()` is `Option`.

Public API: additive. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - starts on main

## Related

- [moq-tokio shapes](/quest/m1/api-tokio-shapes.md) - the merge shape this reuses
- [Auth embedder](/quest/m2/auth-embedder.md) - the admission half of the same surface
- [Server close](/quest/m2/moq-server-close.md) - the listener lifetime that sits beside `with_listeners`
