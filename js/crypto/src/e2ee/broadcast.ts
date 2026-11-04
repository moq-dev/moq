/**
 * Protected broadcasts addressed by semantic track names.
 *
 * The application names tracks the way it would in plaintext (`catalog.json`, `video`). These
 * wrappers derive the physical name of each, so only physical names reach `@moq/net` and the
 * relays behind it.
 *
 * @module
 */
import type { Broadcast as NetBroadcast, Datagram as NetDatagram, Track as NetTrack } from "@moq/net";
import type * as Datagram from "./datagram.ts";
import type * as Epoch from "./epoch.ts";
import type * as Generation from "./generation.ts";
import type * as Track from "./track.ts";

/**
 * The publishing side of a protected broadcast.
 *
 * Wraps the `@moq/net` broadcast at `Credential.path`. Announce that broadcast with
 * `generation.epoch` as its route's instance ({@link Epoch.instance}) once the tracks a
 * subscriber needs first exist.
 */
export class Producer {
	/** The generation this broadcast's tracks are protected under. */
	readonly generation: Generation.Producer;
	#inner: NetBroadcast.Producer;

	/** Protect `inner` under `generation`, taking ownership of it: {@link close} ends it. */
	constructor(inner: NetBroadcast.Producer, generation: Generation.Producer) {
		this.#inner = inner;
		this.generation = generation;
	}

	/** Settles with `null` once the wrapped broadcast closes, as `@moq/net`'s `closed` does. */
	get closed(): NetBroadcast.Producer["closed"] {
		return this.#inner.closed;
	}

	/**
	 * Create the track for `semantic` under its physical name.
	 *
	 * Rejects with {@link Failure} `identity` past 65535 bytes, `reuse` if this generation already
	 * produced the name (even on another broadcast), or the net error if the broadcast already
	 * holds it.
	 */
	async createTrack(semantic: string, info: NetTrack.InfoInit = {}): Promise<Track.Producer> {
		const name = await this.generation.name(semantic);
		return this.generation.produce(this.#inner.createTrack(name, info), semantic);
	}

	/** Create a protected datagram track in its own namespace and sequence space. */
	async createDatagrams(semantic: string, info: Partial<NetDatagram.Info> = {}): Promise<Datagram.Producer> {
		const name = await this.generation.name(semantic);
		return this.generation.produceDatagrams(this.#inner.createDatagrams(name, info), semantic);
	}

	/** End the wrapped broadcast, as `@moq/net`'s `close` does. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}

/**
 * The subscribing side of a protected broadcast.
 *
 * Wraps the `@moq/net` broadcast found at `Credential.path`, bound to the epoch on its route.
 */
export class Consumer {
	/** The generation this broadcast's tracks are read under. */
	readonly generation: Generation.Consumer;
	#inner: NetBroadcast.Consumer;

	/** Read `inner` under `generation`, taking ownership of it: {@link close} releases it. */
	constructor(inner: NetBroadcast.Consumer, generation: Generation.Consumer) {
		this.#inner = inner;
		this.generation = generation;
	}

	/** Settles with `null` once the wrapped broadcast closes, as `@moq/net`'s `closed` does. */
	get closed(): NetBroadcast.Consumer["closed"] {
		return this.#inner.closed;
	}

	/** Subscribe to the track for `semantic` under its physical name; rejects with {@link Failure} `identity` past 65535 bytes. */
	async subscribe(semantic: string, subscription?: NetTrack.Subscription): Promise<Track.Consumer> {
		const name = await this.generation.name(semantic);
		return this.generation.consume(this.#inner.track(name).subscribe(subscription), semantic);
	}

	/** Read the protected datagram track live from now under its semantic name. */
	async datagrams(semantic: string): Promise<Datagram.Consumer> {
		const name = await this.generation.name(semantic);
		return this.generation.consumeDatagrams(this.#inner.datagrams(name), semantic);
	}

	/** Release the wrapped broadcast handle, as `@moq/net`'s `close` does. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}
