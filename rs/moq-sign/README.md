[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-sign

Workspace-only implementation; this crate is not published.

Per-frame Ed25519 signatures for [Media over QUIC](https://moq.dev) broadcasts. Profile
`moq-sign-00` from [draft-lcurley-moq-sign](https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/).

Relays forward and cache signed frames; they cannot forge, retime, or relocate them. Signatures
authenticate content, not liveness: a relay that recorded an instance can replay it whole.

## Shape

An endpoint's `SecretKey` signs; its `PublicKey` is the endpoint ID, displayed as 52 z-base-32
characters (a DNS label). `SecretKey::load_or_generate(path)` keeps one key for the life of a host,
as 52 z-base-32 characters in a `0600` file it never replaces implicitly.

`broadcast::Producer` wraps the `moq-net` broadcast, mints a fresh epoch, and signs each frame
over the application path, epoch, track, group, frame index, and timestamp (at the track's
timescale). Announce the broadcast with that epoch on its route:

```rust
let key = SecretKey::load_or_generate("/var/lib/moq/endpoint.key")?;
let path = moq_net::Path::new(&format!("mesh/{}/agent", key.public()))?;
let publication = origin.create_broadcast(&path)?;
let signed = broadcast::Producer::new(publication.broadcast().clone(), key, path);
let messages = signed.create_track("messages", None)?;
publication.announce(Route::default().with_epoch(signed.epoch().clone()))?;
```

A subscriber wraps the broadcast an origin resolved in a `broadcast::Consumer` with the key it
expects and the same application path; it takes the epoch from the route. A frame reaches the
application only after it verifies. A forged or relocated frame ends its group with
`Error::Signature`, and the track keeps delivering other groups. Every hop must carry the epoch
and timestamps unchanged, which means moq-lite-07. A group's end is not signed: a relay may drop
a group's tail, as MoQ already allows.

A broadcast at the identity level, which trusts the relay's token check alone, uses `moq-net`
directly.

## Processing

Signing is inline and synchronous. `cargo bench -p moq-sign` measures it: a 1 KiB frame costs
about 8 µs to sign and 32 µs to sign and verify through an origin on one core.
