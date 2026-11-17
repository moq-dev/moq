[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-crypto

Cryptography the [Media over QUIC](https://moq.dev) crates share: the `endpoint` module holds
the key that names an endpoint, the `provider` module is the
[rind](https://github.com/kixel-ai/rind) rustls provider, and `cpu` holds the CPU check and its
error.

## Features

`endpoint`, the default, enables that module's secret key. With default features off, the crate is
only `endpoint::PublicKey`, the endpoint ID, with its canonical z-base-32 text: it builds without
`moq-net` or rind, for crates that only name endpoints.
`serde` adds the key's string serialization. `cpu`, enabled by `provider`, adds
only rind. `provider`, off by default, adds the rustls integration without `moq-net`, so
transport crates can use it without a dependency cycle.

## Provider

MoQ runs all of its cryptography on rind. Its primitives panic on a CPU missing a feature rind
requires, so the `cpu` module refuses such a CPU up front. Today that is NEON on aarch64;
every `x86_64` CPU and armv7 are supported, and a CPU without AES or other crypto extensions
runs rind's slower paths rather than being refused:

- `cpu::check()` returns `cpu::UnsupportedCpu`, naming the missing feature.
- `provider::provider()` returns rind's rustls `CryptoProvider`, or that error. It ignores the
  process default, so TLS works whether or not one was installed.
- `provider::install_default()` installs rind as rustls' process default for libraries such as
  reqwest that read it, leaving a provider the process already installed in place.

moq-auth, moq-hls, moq-e2ee, moq-rtmp, and the transports report the same `cpu::UnsupportedCpu`, and
`moq_tokio::crypto` re-exports this module's API.

## Endpoint key

An endpoint's `endpoint::SecretKey` is an Ed25519 key that `SecretKey::load_or_generate(path)` keeps
for the life of a host, as 52 z-base-32 characters in a `0600` file it never replaces implicitly.
`moq-relay` loads it for `--cluster-route-key`, where it converts into `moq_net::origin::RouteKey`.
Its `endpoint::PublicKey` is the endpoint ID, displayed as those 52 characters (a DNS label).
