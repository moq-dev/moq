<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/e2ee

End-to-end encryption for [Media over QUIC](https://moq.dev) groups, datagrams,
and track names. Profile `moq-e2ee-00` from
[draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/),
interoperable with the Rust [`moq-e2ee`](https://crates.io/crates/moq-e2ee) crate.

Relays forward ciphertext. Content keys never enter `@moq/net`.

## Shape

The application distributes a `Credential { context, kid, secret }` over its own
authenticated channel. `credential.produce()` mints a fresh epoch and returns a
`Generation.Producer`, which derives opaque track names and keys for that instance alone.
It accepts no explicit epoch, and claims each track it produces for good. `produce(track)`
resolves once the net track has committed its timescale, so every write is checked against it.

```ts
import { Credential } from "@moq/e2ee";
import { Time } from "@moq/net";

const credential = new Credential({ context, kid, secret });
const generation = credential.produce();
const broadcast = origin.createBroadcast(await credential.path("meeting.hang"));
const video = await generation.produce(
	broadcast.createTrack(await generation.name("video"), { timescale: Time.Timescale.MILLI }),
);
broadcast.announce({ epoch: generation.epoch });

const group = video.appendGroup();
await group.writeFrame(Time.Timestamp.now(), frame);
group.close();
```

A subscriber resolves `credential.path(semantic)`, takes the epoch from the announced
route (or from the application's channel on a wire without route epochs), and calls
`credential.consume(epoch)` to bind a `Generation.Consumer`. Its `consume(subscriber)`
opens a track whose name it derived or read from the decrypted catalog. A consuming
generation cannot produce. Nothing survives a publisher instance: a restart mints a new epoch.

## Processing

WebCrypto AES-GCM is asynchronous, so writes return promises. Each track producer and
consumer runs one serial pump: one AEAD call in flight. Producer net writes land in call
order, so a frame's index always matches its transport position. Await each write for
backpressure. Consumer reads wait for the network outside the pump, so a stalled group
never holds up another group's opens.

A grouped authentication failure ends the track with `Failure` code `authentication`; a
bad datagram is an `authentication` event and the track continues. A consumer keeps a
1024-bit datagram duplicate window below the greatest sequence it opened.
