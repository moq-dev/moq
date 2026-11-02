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
It accepts no explicit epoch, and claims each track it produces for good. `Generation` exports
types only, so nothing in the package can open a producer under a chosen epoch; see the draft's
[epoch rules](https://doc.moq.dev/draft/draft-lcurley-moq-e2ee#epoch).

The application names tracks by their semantic names. `Broadcast.Producer` wraps the
`@moq/net` broadcast and creates each track under its physical name, so only physical names
reach the relays. A physical name is the semantic name encrypted under the generation: a relay
learns only its length rounded up to 16 bytes. `createTrack` resolves once the net track has committed its timescale, so
every write is checked against it. `close()` ends the wrapped broadcast.

```ts
import { Broadcast, Credential } from "@moq/e2ee";
import { Time } from "@moq/net";

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

## Processing

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
