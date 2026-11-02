<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/sign

Per-frame Ed25519 signatures for [Media over QUIC](https://moq.dev) broadcasts. Profile
`moq-sign-00` from [draft-lcurley-moq-sign](https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/),
interoperable with the Rust `moq-sign` crate.

Relays forward and cache signed frames; they cannot forge, retime, or relocate them. Signatures
authenticate content, not liveness: a relay that recorded an instance can replay it whole.

## Shape

An endpoint's `SecretKey` signs; its `PublicKey` is the endpoint ID, displayed as 52 z-base-32
characters (a DNS label). `SecretKey.generate()` keeps the private half a nonextractable `CryptoKey`.

`Broadcast.Producer` wraps the `@moq/net` broadcast, mints a fresh epoch, and signs each frame
over the application path, epoch, track, group, frame index, and timestamp (rounded to the
track's timescale, as the wire carries it). Announce the broadcast with that epoch on its route:

```ts
import { Broadcast, SecretKey } from "@moq/sign";
import { Path } from "@moq/net";

const key = await SecretKey.generate();
const path = Path.from(`mesh/${key.public}/agent`);
const publication = origin.createBroadcast(path);
const signed = new Broadcast.Producer(publication.broadcast, key, path);
const messages = signed.createTrack("messages");
publication.announce({ epoch: signed.epoch });

const group = messages.appendGroup();
await group.writeFrame(undefined, payload);
group.finish();
```

A subscriber wraps the broadcast an origin resolved in a `Broadcast.Consumer` with the key it
expects and the same application path; it takes the epoch from the route. A frame reaches the
application only after it verifies. A forged or relocated frame ends its group with `Failure`
code `signature`, and the track keeps delivering other groups. Every hop must carry the epoch and
timestamps unchanged, which means moq-lite-07. A group's end is not signed: a relay may drop a
group's tail, as MoQ already allows.

## Processing

WebCrypto Ed25519 is asynchronous, so writes return promises. Each track producer runs one serial
pump: one signature in flight, net writes in call order, so a frame's index always matches its
transport position. Await each write for backpressure.
