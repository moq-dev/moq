# [M] moq-relay: io_uring listener cannot authenticate mTLS peers

## Goal

Implement and verify the behavior tracked in [#3112](https://github.com/moq-dev/moq/issues/3112)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`--runtime-io-uring` (#3082) cannot authenticate mTLS peers. The capability is refused at startup rather than silently ignored, so nothing is wrong today, but it is missing.

#### What exists

`moq_uring::quic::server::Config` already carries the knob:

```rust
pub enum ClientAuth {
	None,
	Optional(Vec<X509>),
	Required(Vec<X509>),
}
```

and maps it onto `SslVerifyMode::{PEER, FAIL_IF_NO_PEER_CERT}`. The relay never sets it. `rs/moq-relay/src/uring.rs` refuses the config instead:

```rust
if !listen.tls.root.is_empty() {
	anyhow::bail!(
		"io_uring workers do not implement mTLS client roots (listen.tls.root); ..."
	);
}
```

#### What is missing

`serve_connection` in `rs/moq-relay/src/uring.rs` has no peer-identity branch. Both the `h3` and raw-QUIC arms fall through to the shared auth API:

```rust
let mut params = match &url {
	Some(url) => serve.auth.params_from_url(url),
	None => { /* path + ?jwt= off the SETUP */ }
};
```

There is no equivalent of the `verify_mtls` branch that `Connection::authenticate` takes on the shared tokio path.

Wiring it up means three things:

1. pass `listen.tls.root` into `server::Config::client_auth` instead of bailing;
2. surface the verified peer identity out of `moq_uring::quic::Connection` (the connection is consumed by the handshake today, so this needs new API);
3. take the mTLS branch in `serve_connection` the way the tokio path does.

Note this interacts with #3087: mTLS peers bypass `--auth-api-mode` on the tokio path already, so whichever lands second should not reintroduce the gap on the io\_uring path.

Behind the non-default `io-uring` feature, which PR CI does not compile, so this needs verifying in the moquring container rather than on a PR run.

Split out of #3097 (section 2); section 1 of that issue is #3107. Raised by Codex reviewing #3082 and verified against `dev`.

## Closes

- [#3112](https://github.com/moq-dev/moq/issues/3112) - close this issue when the quest finishes
