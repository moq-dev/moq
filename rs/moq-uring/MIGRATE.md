# Migrating moq-uring

How to update your code for each breaking change, newest first.

## Unreleased

### QUIC limits are `moq_quic::Limits` (#5871)

`quic::Limits` is `moq_quic::Limits`, where each `None` field keeps that limit, and an invalid one fails `set_limits` with `Error::Limits` instead of `WindowRange` or `StreamsRange`.
Start from `quic::Limits::default()` and set only the fields you change.

### The `noq` feature is `quic` (#5664)

The QUIC stack is MoQ's own `moq-quic` instead of `moq-noq-proto`.
Rename the `noq` cargo feature to `quic`.
A qlog directory gets one file per endpoint and one per dial instead of one per connection.

### `CID_LEN` is gone (#5688)

`quic::endpoint::CID_LEN` is removed; endpoints still issue 8-byte connection IDs, sharded or not.

### The server config takes a rustls config (#5312)

`quic::server::Config` drops `identity`, `alpn`, and `client_auth` for one `tls: Arc<rustls::ServerConfig>`, and `Config::new` takes it.
Build it from an identity with `identity.server(&alpns, client_auth)?`, or share a resolver with other listeners.

### Sessions are wrapped for moq-net

moq-net no longer takes a `web_transport_trait` session.
Wrap a moq-uring session with `transport::Session::new`, naming how the request target travels.

```rust
let session = moq_uring::transport::Session::new(connection, moq_net::transport::Binding::Uri);
```
