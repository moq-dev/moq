/**
 * End-to-end encryption for MoQ groups, datagrams, and track names.
 *
 * Implements profile `moq-e2ee-00` from
 * [draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/). Relays
 * forward ciphertext; content keys never enter `@moq/net`. Mirrors the Rust `moq-e2ee` crate.
 *
 * A {@link Credential} is what the application distributes out of band. Each publisher instance
 * calls {@link Credential.produce} to mint a {@link Generation.Producer}, which derives opaque
 * track names and protects `@moq/net` tracks. Subscribers discover the epoch from the broadcast
 * route and bind a {@link Generation.Consumer} with {@link Credential.consume}.
 *
 * ```ts
 * const credential = new Credential({ context, kid, secret });
 * const generation = credential.produce();
 * const broadcast = origin.createBroadcast(await credential.path("meeting.hang"));
 * const video = await generation.produce(broadcast.createTrack(await generation.name("video"), { timescale }));
 * broadcast.announce({ epoch: generation.epoch });
 *
 * const group = video.appendGroup();
 * await group.writeFrame(timestamp, frame);
 * group.close();
 * ```
 *
 * WebCrypto AES-GCM is asynchronous, so writes return promises and run through a serial pump
 * per track: one AEAD call in flight, net writes in call order. Await each write for backpressure.
 *
 * @module
 */

export { Credential } from "./credential.ts";
/** Decrypted datagrams and the events a protected datagram read yields. */
export * as Datagram from "./datagram.ts";
export type { Name } from "./derive.ts";
export { type Code, Failure } from "./failure.ts";
/** Producing generations mint an epoch; consuming generations bind a discovered one. */
export * as Generation from "./generation.ts";
/** Grouped-frame producer and consumer for one group identity. */
export * as Group from "./group.ts";
export { MAX_DATAGRAM_PLAINTEXT, MAX_GROUPED_PLAINTEXT, SECRET_LEN } from "./limits.ts";
/** Grouped-frame and datagram writers and readers for one physical track. */
export * as Track from "./track.ts";
