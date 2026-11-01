[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-e2ee

Workspace-only implementation; this crate is not published.

End-to-end encryption for [Media over QUIC](https://moq.dev) groups, datagrams,
and track names. Profile `moq-e2ee-00` from
[draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).

Relays forward ciphertext. Content keys never enter `moq-net`.

## Shape

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
let broadcast = origin.create_broadcast(&credential.path("meeting.hang")?)?;
let protected = broadcast::Producer::new(broadcast.clone(), generation.clone());
let video = protected.create_track("video", None)?;
broadcast.announce(Route::default().with_epoch(generation.epoch().clone()))?;
```

A subscriber follows `credential.path(semantic)` with `origin.follow(path, None)`. It takes
each instance's epoch from the broadcast it resolved, `broadcast.info().epoch` (or from the
application's channel on a wire older than lite-07), so the key always matches the instance
it reads. It binds the epoch with `credential.consume(epoch)` and wraps the broadcast in a
`broadcast::Consumer`. Its `subscribe(semantic, subscription)` opens the track a decrypted
catalog names. A consuming generation cannot produce. Nothing survives a publisher instance: a restart mints a new epoch.
A cloned process image (a `fork` without `exec`, a restored VM snapshot or checkpoint, or a
producing generation copied to another process) is a new instance too: each copy must call
`produce()` itself, since copies of one generation reuse its nonces.

## Processing

AES-128-GCM is inline and synchronous. `cargo bench -p moq-e2ee` measures the whole
protected write path, including the `moq-net` group: a 1 KiB grouped frame and a
160-byte Opus-sized datagram each cost a few microseconds on one core, so encryption
is a tiny fraction of real time and there is no async pump.

Memory is bounded: a producer retains no ciphertext, and a consumer keeps a 1024-bit
datagram duplicate window below the greatest sequence it opened.
