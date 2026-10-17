# Migrating moq-uring

How to update your code for each breaking change, newest first.

## Unreleased

### The `noq` feature is `quic` (#5664)

The QUIC stack is MoQ's own `moq-quic` instead of `moq-noq-proto`.
A qlog directory gets one file per endpoint and one per dial instead of one per connection.

```toml
# Before
moq-uring = { version = "...", features = ["noq"] }
# After
moq-uring = { version = "...", features = ["quic"] }
```

### `CID_LEN` is gone (#5688)

`quic::endpoint::CID_LEN` is removed; endpoints still issue 8-byte connection IDs, sharded or not.

```rust
// Before
let len = moq_uring::quic::endpoint::CID_LEN;
// After
let len = 8; // endpoints still issue 8-byte connection IDs
```

### The server config takes a rustls config (#5312)

`quic::server::Config` drops `identity`, `alpn`, and `client_auth` for one `tls: Arc<rustls::ServerConfig>`, and `Config::new` takes it.
Build it from an identity with `Identity::server`, or share a resolver with other listeners.

```rust
// Before
let mut config = quic::server::Config::new(identity);
config.alpn = alpns;
config.client_auth = client_auth;
// After
let config = quic::server::Config::new(identity.server(&alpns, client_auth)?);
```

### Sessions are wrapped for moq-net

moq-net no longer takes a `web_transport_trait` session.
Wrap a moq-uring session with `transport::Session::new`, naming how the request target travels.

```rust
let session = moq_uring::transport::Session::new(connection, moq_net::transport::Binding::Uri);
```
