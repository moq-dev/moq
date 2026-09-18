# [M] moq-tokio names sit under their modules

## Goal

moq-tokio's public surface follows the naming rules before its first release
under this name: short names under a module namespace, no `*Config`
compounds at the crate root, nothing named after the adapter it happens to
be today, and no `close()` a caller can forget.

## Plan

The outliers on dev, each with the settled replacement:

- `GoawayConfig` at the crate root (`rs/moq-tokio/src/connection.rs`)
  becomes `connection::Goaway`, and `iroh::EndpointConfig` becomes
  `iroh::Config`.
- `moq_tokio::Duration`, the CLI and TOML parsing newtype, moves to
  `cli::Duration` so the bare name stops shadowing `std::time::Duration`.
- `transport::{Async, AsyncSend, AsyncRecv}` (`rs/moq-tokio/src/transport.rs`)
  keep names that say adapter. `moq_tokio::Transport` (the accepted-session
  enum) and `Request` move under `server::`, so the module and the enum stop
  sharing a word for two things.
- `Connection::close()` is deleted; `abort(err)` covers the explicit case and
  Drop the rest, as its own doc already advises.
- `keep_parse_only` on the seven config types (`tcp`, `quic`, `unix`,
  `listen`, `connect`, `websocket`, `tls`) becomes private to the merge that
  needs it; that merge is `cli::merge`, whose `keep` callback is what
  [moq-tokio shapes](/quest/m1/api-tokio-shapes.md) replaces with a trait.
- `moq_tokio::Deprecated` moves to `cli::Deprecated`; `websocket::Listener::bind_with_alpns`
  has no caller outside its own `bind` and follows `tcp`/`unix` with
  `with_protocols`.
- `watch::FileWatcher` becomes `watch::Files`.
- The `resolved_*` accessors on `connect` and `websocket` configs and
  `quic::Config::resolve() -> Resolved` become one spelling: `resolve()`
  returning a `Resolved` on every config that has effective values.

Public API: breaking on moq-tokio, so on dev. Wire: none. Consumers:
moq-relay, moq-cli, moq-ffi, moq-gst, and the docs under `doc/lib/rs`; run
`just check` across the workspace and the rustdoc lint.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so moq-tokio releases under settled names
- [moq-tokio shapes](/quest/m1/api-tokio-shapes.md) - the type-level fixes that follow the renames
