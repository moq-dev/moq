/**
 * Signed broadcasts, named by the application path both ends agree on.
 *
 * A relay may mount a broadcast under another prefix, so the path a signature binds is the one
 * the application names, never the path the transport reports.
 *
 * @module
 */
import { Identity, type Broadcast as NetBroadcast, type Track as NetTrack, type Path } from "@moq/net";
import { Failure } from "./failure.ts";
import type { PublicKey, SecretKey } from "./key.ts";
import { Scope } from "./scope.ts";
import * as Track from "./track.ts";

/**
 * The publishing side of a signed broadcast.
 *
 * Mints a fresh identity, so a restart never signs a sequence an earlier instance already signed.
 * Announce the wrapped broadcast with {@link identity} on its route.
 */
export class Producer {
	/** The identity this instance signs under, to announce on the broadcast's route. */
	readonly identity: Identity.Valid = Identity.random();
	#inner: NetBroadcast.Producer;
	#key: SecretKey;
	#path: Path.Valid;
	// Each track's lowest unsigned sequence, kept across the track's replacements.
	#sequences = new Map<string, Track.Sequences>();

	/** Sign `inner` with `key`, as the broadcast at application path `path`, taking ownership of it: {@link close} ends it. */
	constructor(inner: NetBroadcast.Producer, key: SecretKey, path: Path.Valid) {
		this.#inner = inner;
		this.#key = key;
		this.#path = path;
	}

	/** Settles with `null` once the wrapped broadcast closes, as `@moq/net`'s `closed` does. */
	get closed(): NetBroadcast.Producer["closed"] {
		return this.#inner.closed;
	}

	/** Create a track whose frames are signed; throws the net error if the broadcast already holds it. */
	createTrack(name: string, info: Partial<NetTrack.Info> = {}): Track.Producer {
		const inner = this.#inner.createTrack(name, info);
		const timescale = info.timescale ?? undefined;
		let sequences = this.#sequences.get(name);
		if (!sequences) {
			sequences = { next: 0 };
			this.#sequences.set(name, sequences);
		}
		return Track.Producer.make(inner, this.#key, new Scope(this.#path, this.identity, name), sequences, timescale);
	}

	/** End the wrapped broadcast, as `@moq/net`'s `close` does. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}

/**
 * The subscribing side of a signed broadcast.
 *
 * Every frame must verify against the expected publisher key under the identity of the route the
 * broadcast was resolved through. The route comes from the relay, so this checks that frames
 * belong together, not that the instance is live: a relay can replay a recorded instance whole.
 */
export class Consumer {
	/** The identity frames must be signed under. */
	readonly identity: Identity.Valid;
	#inner: NetBroadcast.Consumer;
	#key: PublicKey;
	#path: Path.Valid;

	/**
	 * Verify `inner` against `key`, as the broadcast at application path `path`, taking ownership
	 * of it: {@link close} releases it. Throws {@link Failure} `identity` if `inner` was not resolved
	 * through a route that carries its publisher's identity: one a relay minted for a wire without
	 * identities names the announcement, not the publisher.
	 */
	constructor(inner: NetBroadcast.Consumer, key: PublicKey, path: Path.Valid) {
		const identity = inner.identity;
		if (!identity || Identity.isMinted(identity)) throw new Failure("identity");
		this.identity = identity;
		this.#inner = inner;
		this.#key = key;
		this.#path = path;
	}

	/** Settles with `null` once the wrapped broadcast closes, as `@moq/net`'s `closed` does. */
	get closed(): NetBroadcast.Consumer["closed"] {
		return this.#inner.closed;
	}

	/** Subscribe to a track whose frames must verify. */
	subscribe(name: string, subscription?: NetTrack.Subscription): Track.Consumer {
		const inner = this.#inner.track(name).subscribe(subscription);
		return Track.Consumer.make(inner, this.#key, new Scope(this.#path, this.identity, name));
	}

	/** Release the wrapped broadcast handle, as `@moq/net`'s `close` does. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}
