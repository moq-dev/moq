<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/crypto

Content signing and end-to-end encryption for [Media over QUIC](https://moq.dev), mirroring the
Rust `moq-crypto` crate. `Sign` signs every frame so relays cannot forge it, and `E2ee` encrypts
groups, datagrams, and track names so relays cannot read them. Both wrap `@moq/net` and refuse a
timestamp its track would refuse before signing or encrypting.

## Signing

Per-frame Ed25519 signatures for [Media over QUIC](https://moq.dev) broadcasts. Profile
`moq-sign-00` from [draft-lcurley-moq-sign](https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/),
interoperable with Rust's `moq_crypto::sign`.

Relays forward and cache signed frames; they cannot forge, retime, or relocate them. Signatures
authenticate content, not liveness: a relay that recorded an instance can replay it whole.

### Shape

An endpoint's `SecretKey` signs; its `PublicKey` is the endpoint ID, displayed as 52 z-base-32
characters (a DNS label). `SecretKey.generate()` keeps the private half a nonextractable `CryptoKey`.

`Broadcast.Producer` wraps the `@moq/net` broadcast, mints a fresh instance, and signs each frame
over the application path, instance, track, group, frame index, and timestamp (rounded to the
track's timescale, as the wire carries it). Announce the broadcast with that instance on its route:

```ts
import { Sign } from "@moq/crypto";
import { Path } from "@moq/net";

const { Broadcast, SecretKey } = Sign;

const key = await SecretKey.generate();
const path = Path.from(`mesh/${key.public}/agent`);
const publication = origin.createBroadcast(path);
const signed = new Broadcast.Producer(publication.broadcast, key, path);
const messages = signed.createTrack("messages");
publication.announce({ instance: signed.instance });

const group = messages.appendGroup();
await group.writeFrame(undefined, payload);
group.finish();
```

A subscriber wraps the broadcast an origin resolved in a `Broadcast.Consumer` with the key it
expects and the same application path; it takes the instance from the route. A frame reaches the
application only after it verifies. A forged or relocated frame ends its group with `Failure`
code `signature`, and the track keeps delivering other groups. Every hop must carry the instance and
timestamps unchanged, which means moq-lite-07. A group's end is not signed: a relay may drop a
group's tail, as MoQ already allows.

### Processing

WebCrypto Ed25519 is asynchronous, so writes return promises. Each track producer runs one serial
pump: one signature in flight, net writes in call order, so a frame's index always matches its
transport position. Await each write for backpressure.

## Encryption

End-to-end encryption for [Media over QUIC](https://moq.dev) groups, datagrams,
and track names. Profile `moq-e2ee-00` from
[draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/),
interoperable with Rust's `moq_crypto::e2ee`.

Relays forward ciphertext. Content keys never enter `@moq/net`.

### Shape

The application distributes a `Credential { context, kid, secret }` over its own
authenticated channel. `credential.produce()` mints a fresh epoch and returns a
`Generation.Producer`, which derives opaque track names and keys for that instance alone.
It accepts no explicit epoch, and claims each track it produces for good. `Generation` exports
types only, so nothing in the package can open a producer under a chosen epoch; see the draft's
[epoch rules](https://doc.moq.dev/draft/draft-lcurley-moq-e2ee#epoch).

The application names tracks by their semantic names. `Broadcast.Producer` wraps the
`@moq/net` broadcast and creates each track under its physical name, so only physical names
reach the relays. A physical name is the semantic name encrypted under the generation: a relay
learns only its length rounded up to 16 bytes. `createTrack` resolves once the net track has committed its timescale, so
every write is checked against it. `close()` ends the wrapped broadcast.

```ts
import { E2ee } from "@moq/crypto";
import { Time } from "@moq/net";

const { Broadcast, Credential } = E2ee;

const credential = new Credential({ context, kid, secret });
const generation = credential.produce();
const publication = origin.createBroadcast(await credential.path("meeting.hang"));
const video = await new Broadcast.Producer(publication.broadcast, generation).createTrack("video", {
	timescale: Time.Timescale.MILLI,
});
publication.announce({ epoch: generation.epoch });

const group = video.appendGroup();
await group.writeFrame(Time.Timestamp.now(), frame);
group.finish();
video.finish();
video.close();
```

A subscriber follows `credential.path(semantic)` with `origin.follow(path)`. It takes each
instance's epoch from the broadcast it resolved, `broadcast.epoch` (or from the application's
channel on a wire without route epochs), so the key always matches the instance it reads. It
binds the epoch with `credential.consume(epoch)` and wraps the broadcast in a
`Broadcast.Consumer`. Its
`subscribe(semantic)` opens the track a decrypted catalog names, and `close()` releases the
wrapped broadcast. A consuming generation
cannot produce. Nothing survives a publisher instance: a restart mints a new epoch.
A cloned process image (a `fork` without `exec`, a restored VM snapshot or checkpoint, or a
producing generation copied to another process) is a new instance too: each copy must call
`produce()` itself, since copies of one generation reuse its nonces.

### Processing

WebCrypto AES-GCM is asynchronous, so writes return promises. Each track producer and
consumer runs one serial pump: one AEAD call in flight. Producer net writes land in call
order, so a frame's index always matches its transport position. Await each write for
backpressure. Consumer reads wait for the network outside the pump, so a stalled group
never holds up another group's opens.

Finish each group with `group.finish()` and declare the track's end with `track.finish()`.
Groups already created remain writable until they finish or the track is released.
`track.close()` releases the producer, draining a declared end; without `finish()` it
cancels the unfinished track. Use `abort(error)` on a group or track to end it with an
error, and await its `closed` outcome to observe completion. A group finish drains its
queued writes unless the track aborts. A track's declared end remains abortable until
its outcome settles or the producer is released.

A grouped authentication failure ends the track with `Failure` code `authentication`; a
datagram that fails to open is dropped as an `identity`, `oversize`, or `authentication`
event and the track continues. Every subscription through
one consuming generation shares each key's AEAD budget, so resubscribing cannot reset it. A consumer keeps a
1024-bit datagram duplicate window below the greatest sequence it opened.
