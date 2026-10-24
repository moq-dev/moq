# [XS] io_uring listeners honor listen.extensions

## Goal

A relay listening on io_uring offers the `[listen] extensions` its operator
configured, as the tokio listeners already do.

## Plan

`rs/moq-relay/src/uring.rs` builds `moq_net::Server::new().with_versions(...)`
and never applies `listen.extensions`; the tokio path
(`rs/moq-relay/src/relay.rs`, `websocket.rs`) does. Apply them and add a uring
test that a configured extension is negotiated.
[Relay config](/quest/m1/api-audit/relay-config.md) keeps extensions as the
operator's interop knob, so every listener must honor it.

Public API: none. Wire: none.
