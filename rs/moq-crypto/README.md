[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-crypto

Cryptography for [Media over QUIC](https://moq.dev): the `e2ee` module encrypts groups,
datagrams, and track names so relays cannot read them. It wraps `moq-net` and checks timestamps
with its `track::Info::rescale`, so a frame it accepts is one `moq-net` accepts. The `endpoint`
module holds the key that names an endpoint. The `provider` module is the
[rind](https://github.com/kixel-ai/rind) rustls provider; `cpu` holds the CPU check and error
that the MoQ crates share.

## Features

`endpoint` and `e2ee`, both default, enable those modules. With default features off, the crate is
only `endpoint::PublicKey`, the endpoint ID, with its canonical z-base-32 text: it builds without
`moq-net` or rind, for crates that only name endpoints.
`serde` adds the key's string serialization. `cpu`, enabled by `e2ee` and `provider`, adds
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

moq-auth, moq-hls, e2ee, moq-rtmp, and the transports report the same `cpu::UnsupportedCpu`, and
`moq_tokio::crypto` re-exports this module's API.

## Endpoint key

An endpoint's `endpoint::SecretKey` is an Ed25519 key that `SecretKey::load_or_generate(path)` keeps
for the life of a host, as 52 z-base-32 characters in a `0600` file it never replaces implicitly.
`moq-relay` loads it for `--cluster-route-key`, where it converts into `moq_net::origin::RouteKey`.
Its `endpoint::PublicKey` is the endpoint ID, displayed as those 52 characters (a DNS label).

## Encryption

End-to-end encryption for [Media over QUIC](https://moq.dev) groups, datagrams,
and track names. Profile `moq-e2ee-00` from
[draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).

Relays forward ciphertext. Content keys never enter `moq-net`.

### Shape

The application distributes a `Credential { context, kid, secret }` over its own
authenticated channel. `credential.produce()` mints a fresh epoch and returns a
`generation::Producer`, which derives opaque track names and keys for that instance alone.
It accepts no explicit epoch; its clones share permanent track claims.

The application names tracks by their semantic names. `broadcast::Producer` wraps the
`moq-net` broadcast and creates each track under its physical name, so only physical names
reach the relays. A physical name is the semantic name encrypted under the generation: a relay
learns only its length rounded up to 16 bytes. A track served on demand is reserved with
`reserve_track` and accepted once its demand shows a subscriber.

```rust
let credential = Credential::new(credential::Config { context, kid, secret })?;
let generation = credential.produce();
// The route serves the generation's epoch as its instance, so subscribers find the key.
let options = RouteOptions::hidden().with_instance(generation.epoch().instance(), 0);
let route = origin.route(&credential.path("meeting.hang")?, options)?;
let protected = broadcast::Producer::new(route.broadcast().clone(), generation.clone());
let video = protected.create_track("video", None)?;
route.announce()?;
```

A subscriber follows `credential.path(semantic)` with `origin.follow(path, None)`. It takes
each instance's epoch from the broadcast it resolved, `Epoch::of(&broadcast)` (or from the
application's channel on a wire that carries no instance, older than lite-07), so the key always matches the instance
it reads. It binds the epoch with `credential.consume(epoch)` and wraps the broadcast in a
`broadcast::Consumer`. Its `subscribe(semantic, subscription)` opens the track a decrypted
catalog names. A consuming generation cannot produce. Nothing survives a publisher instance: a restart mints a new epoch.
A cloned process image (a `fork` without `exec`, a restored VM snapshot or checkpoint, or a
producing generation copied to another process) is a new instance too: each copy must call
`produce()` itself, since copies of one generation reuse its nonces.

### Processing

AES-128-GCM is inline and synchronous. `cargo bench -p moq-crypto --bench protect` measures the whole
protected write path, including the `moq-net` group: a 1 KiB grouped frame and a
160-byte Opus-sized datagram each cost a few microseconds on one core, so encryption
is a tiny fraction of real time and there is no async pump.

Memory is bounded: a producer retains no ciphertext, and a consumer keeps a 1024-bit
datagram duplicate window below the greatest sequence it opened.
