[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-crypto

Cryptography for [Media over QUIC](https://moq.dev): the `sign` module signs every frame so
relays cannot forge it, and the `e2ee` module encrypts groups, datagrams, and track names so
relays cannot read them. Both wrap `moq-net` and check timestamps with its
`track::Info::rescale`, so a frame either half accepts is one `moq-net` accepts. The `provider`
module is the [rind](https://github.com/moq-dev/rind) rustls provider and CPU check that the
MoQ transport crates share.

## Features

`sign` and `e2ee`, both default, enable the two modules. With default features off, the crate is
only `sign::PublicKey`, the endpoint ID, with its canonical z-base-32 text: it builds without
`moq-net` or rind, for crates such as `moq-auth` that name endpoints but sign nothing.
`serde` adds the key's string serialization. `provider`, off by default, enables the `provider`
module; it depends only on rind and rustls, never on `moq-net`, so the transport crates can
use it without a dependency cycle.

## Provider

MoQ runs all of its cryptography on rind, which needs certain CPU features (for example AES
instructions on `x86_64`). Its primitives panic on a CPU without them, so the `provider` module
refuses such a CPU up front:

- `provider::check()` returns `UnsupportedCpu`, naming the missing feature.
- `provider::provider()` returns rind's rustls `CryptoProvider`, or that error. It ignores the
  process default, so TLS works whether or not one was installed.
- `provider::install_default()` installs rind as rustls' process default for libraries such as
  reqwest that read it, leaving a provider the process already installed in place.

moq-quic, moq-tokio, qmux, and moq-uring report the same `UnsupportedCpu`, and
`moq_tokio::crypto` re-exports this module's API.

## Signing

Per-frame Ed25519 signatures for [Media over QUIC](https://moq.dev) broadcasts. Profile
`moq-sign-00` from [draft-lcurley-moq-sign](https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/).

Relays forward and cache signed frames; they cannot forge, retime, or relocate them. Signatures
authenticate content, not liveness: a relay that recorded an instance can replay it whole.

### Shape

An endpoint's `SecretKey` signs; its `PublicKey` is the endpoint ID, displayed as 52 z-base-32
characters (a DNS label). `SecretKey::load_or_generate(path)` keeps one key for the life of a host,
as 52 z-base-32 characters in a `0600` file it never replaces implicitly.

`broadcast::Producer` wraps the `moq-net` broadcast, mints a fresh instance, and signs each frame
over the application path, instance, track, group, frame index, and timestamp (at the track's
timescale). Announce the broadcast with that instance on its route:

```rust
let key = SecretKey::load_or_generate("/var/lib/moq/endpoint.key")?;
let path = moq_net::Path::new(&format!("mesh/{}/agent", key.public()))?;
let publication = origin.create_broadcast(&path)?;
let signed = broadcast::Producer::new(publication.broadcast().clone(), key, path);
let messages = signed.create_track("messages", None)?;
publication.announce(Route::default().with_instance(signed.instance().clone(), 0))?;
```

A subscriber wraps the broadcast an origin resolved in a `broadcast::Consumer` with the key it
expects and the same application path; it takes the instance from the route. A frame reaches the
application only after it verifies. A forged or relocated frame ends its group with
`Error::Signature`, and the track keeps delivering other groups. Every hop must carry the instance
and timestamps unchanged, which means moq-lite-07. A group's end is not signed: a relay may drop
a group's tail, as MoQ already allows.

A broadcast at the identity level, which trusts the relay's token check alone, uses `moq-net`
directly.

### Processing

Signing is inline and synchronous. `cargo bench -p moq-crypto --bench sign` measures it: a 1 KiB frame costs
about 8 µs to sign and 32 µs to sign and verify through an origin on one core.

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
let publication = origin.create_broadcast(&credential.path("meeting.hang")?)?;
let protected = broadcast::Producer::new(publication.broadcast().clone(), generation.clone());
let video = protected.create_track("video", None)?;
publication.announce(Route::default().with_instance(generation.epoch().instance(), 0))?;
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
