[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-crypto

Cryptography the [Media over QUIC](https://moq.dev) crates share: the `provider` module is the
[rind](https://github.com/kixel-ai/rind) rustls provider, and `cpu` holds the CPU check and its
error.

## Features

Both are off by default. `cpu` adds only rind. `provider` enables it and adds the rustls
integration, so transport crates can use it without a dependency cycle.

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
