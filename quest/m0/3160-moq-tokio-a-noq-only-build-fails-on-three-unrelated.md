# [M] moq-tokio: a noq-only build fails on three unrelated errors, because no crypto provider is…

## Goal

Implement and verify the behavior tracked in [#3160](https://github.com/moq-dev/moq/issues/3160)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`cargo check -p moq-tokio --no-default-features --features noq` fails on `dev` with three errors that never name the cause:

```
error[E0599]: no function or associated item named `default` found for struct `EndpointConfig`
  --> rs/moq-tokio/src/noq.rs:275:46
error[E0599]: no function or associated item named `with_crypto` found for struct `web_transport_noq::noq::ServerConfig`
  --> rs/moq-tokio/src/noq.rs:518:36
error[E0599]: no function or associated item named `default` found for struct `EndpointConfig`
  --> rs/moq-tokio/src/noq.rs:537:50
```

#### Cause

`noq` alone selects no crypto provider. The manifest routes providers into the backends through separate features, with default features off on each backend dependency:

```toml
aws-lc-rs = ["rustls/aws-lc-rs", "rcgen?/aws_lc_rs", "quinn?/rustls-aws-lc-rs", "web-transport-noq?/aws-lc-rs"]
```

Without `aws-lc-rs` or `ring`, `web-transport-noq` is built without its own provider and the constructors `noq.rs` calls stop existing. `--features noq,aws-lc-rs` is clean.

Unlike `quinn`, this is not a runtime question. `quinn` compiles either way and `crypto::provider()` accepts a provider the application installed with `CryptoProvider::install_default()`, which is a supported path and must keep working. `noq` cannot get that far: the build fails before any application code runs, so nothing installed later can rescue it. `quiche` needs neither, bringing its own through boringssl.

#### Why the matrix misses it

`just rs tokio-features` (added in #3150) checks `--no-default-features` and `--all-features`. Neither selects a lone backend, and workspace builds never do either, since cargo unifies features per crate and any dependent enabling a provider enables it for everyone. So only an explicit `-p moq-tokio --features noq`, or an external consumer picking that combination with `default-features = false`, reaches it.

#### Options

- A `compile_error!` gated on `all(feature = "noq", not(any(feature = "aws-lc-rs", feature = "ring")))`, naming the two features. One honest error instead of three unrelated ones. It rejects nothing that currently builds, since the build already fails.
- Or have the `noq` feature pull a default provider itself, which makes the combination work rather than explaining it, at the cost of a provider choice the consumer did not make.

Either way it is worth extending `tokio-features` to cover each backend on its own, since the same shape can appear for any of them.

Found while preparing #3152, which is closed as superseded by #3150; this is the part of it that still applies. Verified against `dev` at 1c02995.

## Closes

- [#3160](https://github.com/moq-dev/moq/issues/3160) - close this issue when the quest finishes
