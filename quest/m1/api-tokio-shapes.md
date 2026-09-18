# [M] moq-tokio types make the wrong call impossible

## Goal

moq-tokio's first release under this name has shapes the type system
enforces, not doc comments that warn: a listener that closes gracefully when
dropped, a worker whose server and spawner cannot be cross-wired, config
fields typed in `std::time::Duration`, one construction idiom, and no
callback or six-argument merge. The crate is new on main (moq-native is a
tombstone), so every break here is free.

## Plan

- `impl Drop for Listener` does the synchronous half of `shutdown()`
  (`endpoint.close()`); `async fn close(self)` stays as the opt-in that waits
  the grace period. Today `rs/moq-tokio/src/server.rs` has no `Drop`, so an
  early return skips the graceful close entirely.
- `worker::Group::members()` returns `Vec<worker::Member<'_>>` with
  `Member::serve(self, make)` consuming the pair; today it returns
  `(Server, Spawner)` tuples and `Spawner::serve(server, make)` accepts any
  server, so worker 1's driver on worker 0's thread compiles. `Workers::bind`
  takes `server::Config` plus `worker::Config`, not three configs.
- `cli::merge` (five arguments) becomes a `cli::Merge` struct with
  `apply(self, parsed)`. The keep closure and `keep_parse_only` methods are
  already gone. `pub use usage;` with the same one-line doc `notify` carries,
  since `merge` and `answer` take and return `usage` types.
- Every config duration field is `std::time::Duration`; the humantime
  newtype stays private to parsing. Embedders write `Duration::ZERO.into()`
  today (moq-gst, moq.pro). Then delete `Backoff::{initial, multiplier, max,
  timeout}` and `connection::Goaway::redirect` (accessors that duplicate the
  field or have no caller) and fold `handover(Option<Duration>)` into a
  `Resolved`.
- One construction idiom: `connect::Config` and `listen::Config` are
  `#[non_exhaustive]` with public fields (no struct literal), while
  `client::Config` and `server::Config` add `with_*` builders. Drop the
  builders and document `Default` plus assignment once. `Server`'s
  `with_websocket/with_iroh/with_publisher/with_subscriber/with_stats` become
  `server::Config` fields.
- `listen::Config::bind: Option<String>` becomes a `listen::Bind` enum
  (`Addr(SocketAddr)` or `Host(String, u16)`) so `fly-global-services:443`
  fails at load, not at bind; `lb_id` plus `lb_nonce` become one
  `Option<quic::LoadBalancer { id, nonce }>`, deleting `LbNonceWithoutId`.
- `Request::close(self, code: u16)` becomes `Request::reject(self, Reject)`
  with `Reject::{Unauthorized, Forbidden, App(u16)}`; the verb collides with
  `Listener::close` and the u16 is decoded back into an enum inside.
- Cosmetic, in the same pass: `moq_tokio::Deprecated` moves to
  `cli::Deprecated`; `websocket::Listener::bind_with_alpns` (no external
  caller) matches `tcp`/`unix` with `with_protocols`; `failover::Failure<E>`
  becomes crate-private or is renamed so it stops reading as
  `accept::Failure`; `moq_tokio::Transport` and `Request` live under
  `server::`. The adapter types are already `transport::{Session, SendStream,
  RecvStream}`.
- `moq_tokio::crypto::install_default()` so embedders stop copying the
  aws-lc-rs provider boilerplate (moq.pro has it in three binaries).

Public API: breaking on moq-tokio, so on dev. Wire: none. Consumers:
moq-relay, moq-cli, moq-ffi, moq-gst, libmoq tests, moq.pro's edge.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - the release that follows is moq-tokio's first
- [Relay embedding](/quest/m2/relay-embed.md) - the embedder surface that builds on these configs
