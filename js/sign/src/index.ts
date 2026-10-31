/**
 * Per-frame Ed25519 signatures for MoQ broadcasts.
 *
 * Implements profile `moq-sign-00` from
 * [draft-lcurley-moq-sign](https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/). A publisher
 * signs every grouped frame with its endpoint's {@link SecretKey}, binding the broadcast path, its
 * publisher epoch, the track, the group, the frame index, and the timestamp. A subscriber hands a
 * frame to the application only after it verifies against the {@link PublicKey} it expects, so a
 * relay can proxy and cache without forging, retiming, relocating, or splicing content. Signatures
 * authenticate content, not liveness: a relay that recorded an instance can replay it whole under
 * its epoch. Mirrors the Rust `moq-sign` crate.
 *
 * ```ts
 * const key = await SecretKey.generate();
 * const path = Path.from(`mesh/${key.public}/agent`);
 * const publication = origin.createBroadcast(path);
 * const signed = new Broadcast.Producer(publication.broadcast, key, path);
 * const messages = signed.createTrack("messages");
 * publication.announce({ epoch: signed.epoch });
 *
 * const group = messages.appendGroup();
 * await group.writeFrame(undefined, payload);
 * group.finish();
 * ```
 *
 * WebCrypto Ed25519 is asynchronous, so writes return promises and run through a serial pump
 * per track: one signature in flight, net writes in call order. Await each write for backpressure.
 *
 * A broadcast that needs no signature, the identity level, uses `@moq/net` directly.
 *
 * @module
 */

export * as Broadcast from "./broadcast.ts";
export { type Code, Failure } from "./failure.ts";
/** Signing writer and verifying reader for one group. */
export * as Group from "./group.ts";
export { PublicKey, SecretKey } from "./key.ts";
/** Signing producer and verifying subscriber for one track. */
export * as Track from "./track.ts";
