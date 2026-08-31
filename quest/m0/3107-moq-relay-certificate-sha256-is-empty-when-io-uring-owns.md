# [S] moq-relay: /certificate.sha256 is empty when io_uring owns QUIC

## Goal

Implement and verify the behavior tracked in [#3107](https://github.com/moq-dev/moq/issues/3107)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

In `Relay::load` (`rs/moq-relay/src/relay.rs`) the certificate handle the web endpoint serves is chosen from the tokio workers or the shared server:

```rust
// The workers hold the certificates when they own QUIC; the server has none.
let certificates = match &workers {
    Some(workers) => workers.certificates(),
    None => server.certificates(),
};
```

When `runtime.io_uring` is set, `workers` is `None` (the io\_uring workers live in `uring` instead) and the shared server was made stream-only, so this falls through to an empty `server.certificates()`. `/certificate.sha256` then returns nothing, and a browser that pins a self-signed relay certificate through that endpoint cannot connect.

`crate::uring::Workers` has no `certificates()` at all today, and `moq_tokio::tls::Certificates::new` is `pub(crate)`, so exposing this needs a public constructor in moq-tokio plus the fingerprints of the chain the io\_uring workers actually loaded.

Note that the io\_uring path refuses `listen.tls.generate` and requires a certificate file, so this only bites a file-provided self-signed certificate, which is exactly the case the endpoint exists for.

Found by Codex review on #3082, deferred because `rs/moq-relay/src/uring.rs` sits behind the non-default `io-uring` feature and is not compiled by PR CI.

## Closes

- [#3107](https://github.com/moq-dev/moq/issues/3107) - close this issue when the quest finishes
