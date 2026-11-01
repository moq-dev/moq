/**
 * End-to-end encryption for MoQ groups, datagrams, and track names.
 *
 * Implements profile `moq-e2ee-00` from
 * [draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/). Relays
 * forward ciphertext; content keys never enter `@moq/net`. Mirrors the Rust `moq-e2ee` crate.
 *
 * A {@link Credential} is what the application distributes out of band. Each publisher instance
 * calls {@link Credential.produce} to mint a {@link Generation.Producer} and wraps its `@moq/net`
 * broadcast in a {@link Broadcast.Producer}. Subscribers follow the path, take each instance's
 * epoch from the broadcast it resolved, bind a {@link Generation.Consumer} with
 * {@link Credential.consume}, and wrap the broadcast in a {@link Broadcast.Consumer}. Both sides name tracks by their semantic names; only the
 * derived physical names reach `@moq/net`.
 *
 * ```ts
 * const credential = new Credential({ context, kid, secret });
 * const generation = credential.produce();
 * const publication = origin.createBroadcast(await credential.path("meeting.hang"));
 * const video = await new Broadcast.Producer(publication.broadcast, generation).createTrack("video", { timescale });
 * publication.announce({ epoch: generation.epoch });
 *
 * const group = video.appendGroup();
 * await group.writeFrame(timestamp, frame);
 * group.finish();
 * video.finish();
 * video.close();
 * ```
 *
 * WebCrypto AES-GCM is asynchronous, so writes return promises and run through a serial pump
 * per track: one AEAD call in flight, net writes in call order. Await each write for backpressure.
 *
 * @module
 */

export * as Broadcast from "./broadcast.ts";
export { Credential } from "./credential.ts";
/** Decrypted datagrams and the events a protected datagram read yields. */
export * as Datagram from "./datagram.ts";
export type { Name } from "./derive.ts";
export { type Code, Failure } from "./failure.ts";
/** Producing generations mint an epoch; consuming generations bind a discovered one. Types only: a {@link Credential} makes them. */
export type * as Generation from "./generation.ts";
/** Grouped-frame producer and consumer for one group identity. */
export * as Group from "./group.ts";
export { MAX_DATAGRAM_PLAINTEXT, MAX_GROUPED_PLAINTEXT, SECRET_LEN } from "./limits.ts";
/** Grouped-frame and datagram writers and readers for one physical track. */
export * as Track from "./track.ts";
