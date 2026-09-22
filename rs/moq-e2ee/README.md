[![Documentation](https://docs.rs/moq-e2ee/badge.svg)](https://docs.rs/moq-e2ee/)
[![Crates.io](https://img.shields.io/crates/v/moq-e2ee.svg)](https://crates.io/crates/moq-e2ee)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-e2ee

End-to-end encryption for [Media over QUIC](https://moq.dev) groups, datagrams,
and track names. Profile `moq-e2ee-00` from
[draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).

Relays forward ciphertext. Content keys never enter `moq-net`. The TypeScript twin is `@moq/e2ee`.

```bash
cargo add moq-e2ee
```

## Shape

The application distributes a `Credential { context, kid, secret }` over its own
authenticated channel. Every publisher instance mints an `Epoch` and binds it as a
`Generation`, which derives the opaque track names and keys for that instance alone:

```rust
let credential = Credential::new(credential::Config { context, kid, secret })?;
let generation = credential.generation(Epoch::mint());
let path = credential.path("meeting.hang")?.join(generation.epoch().as_str());
let name = generation.name("video")?;
let producer = generation.produce(broadcast.create_track(name.as_str(), None)?)?;
```

A subscriber discovers instances under `credential.path(semantic)`, takes the greatest
epoch from the last path segment, binds the same generation, and calls
`generation.consume(subscriber)` on a track whose name it derived or read from the
decrypted catalog. Nothing survives a publisher instance: a restart mints a new epoch.

## Processing

AES-128-GCM is inline and synchronous. `cargo bench -p moq-e2ee` measures the whole
protected write path, including the `moq-net` group: a 1 KiB grouped frame and a
160-byte Opus-sized datagram each cost a few microseconds on one core, so encryption
is a tiny fraction of real time and there is no async pump.

Memory is bounded: a producer retains no ciphertext, and a consumer keeps a 1024-bit
datagram duplicate window below the greatest sequence it opened.
