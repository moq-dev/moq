[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-crypto

Cryptography the [Media over QUIC](https://moq.dev) crates share: the `provider` module is the
[rind](https://github.com/kixel-ai/rind) rustls provider, and `cpu` re-exports rind's DIT guard.

## Features

Both are off by default. `cpu` adds only rind. `provider` enables it and adds the rustls
integration, so transport crates can use it without a dependency cycle.

## Provider

MoQ runs all of its cryptography on rind, which requires only the CPU features its target
enables at compile time, so it runs on every CPU the target does:

- `provider::provider()` returns rind's rustls `CryptoProvider`. It ignores the process
  default, so TLS works whether or not one was installed.
- `provider::install_default()` installs rind as rustls' process default for libraries such as
  reqwest that read it, leaving a provider the process already installed in place.

`moq_tokio::crypto` re-exports this module's API.
