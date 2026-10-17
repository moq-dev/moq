/**
 * Producing generations mint an epoch; consuming generations bind a discovered one.
 *
 * @module
 */
import type { Epoch, Track as NetTrack } from "@moq/net";
import type { Credential } from "./credential.ts";
import { DATAGRAM, encode, GROUP, type Name, nameInfo, parse } from "./derive.ts";
import { Failure } from "./failure.ts";
import type { Keys } from "./internal.ts";
import { trackKey } from "./key.ts";
import { expand } from "./secret.ts";
import * as Track from "./track.ts";

function keys(credential: Credential, epoch: Epoch.Valid, name: Name): Keys {
	return {
		group: trackKey(credential, epoch, name, GROUP),
		datagram: trackKey(credential, epoch, name, DATAGRAM),
	};
}

/** A consume-only generation bound to a discovered epoch. */
export class Consumer {
	/** The credential this generation derives from. */
	readonly credential: Credential;
	/** The epoch this generation is scoped to. */
	readonly epoch: Epoch.Valid;

	private constructor(credential: Credential, epoch: Epoch.Valid) {
		this.credential = credential;
		this.epoch = epoch;
	}

	/** @internal */
	static make(credential: Credential, epoch: Epoch.Valid): Consumer {
		return new Consumer(credential, epoch);
	}

	/** The opaque physical track name for a semantic track name; rejects with {@link Failure} `identity` past 65535 bytes. */
	async name(semantic: string): Promise<Name> {
		const { context, kid } = this.credential;
		return encode(await expand(this.credential, nameInfo(context, this.epoch, kid, semantic)));
	}

	/** Open a net subscription whose track name is a physical name; throws {@link Failure} `identity` if it is not one. */
	consume(track: NetTrack.Subscriber): Track.Consumer {
		return Track.Consumer.make(track, keys(this.credential, this.epoch, parse(track.name)));
	}
}

/**
 * A publisher-minted generation. It claims each physical track it produces for good, so
 * reopening a track can never restart its nonce counters.
 */
export class Producer {
	/** The credential this generation derives from. */
	readonly credential: Credential;
	/** The epoch minted for this publisher instance. */
	readonly epoch: Epoch.Valid;

	#consumer: Consumer;
	#claims = new Set<Name>();

	private constructor(credential: Credential, epoch: Epoch.Valid) {
		this.credential = credential;
		this.epoch = epoch;
		this.#consumer = Consumer.make(credential, epoch);
	}

	/** @internal */
	static make(credential: Credential, epoch: Epoch.Valid): Producer {
		return new Producer(credential, epoch);
	}

	/** The opaque physical track name for a semantic track name; rejects with {@link Failure} `identity` past 65535 bytes. */
	name(semantic: string): Promise<Name> {
		return this.#consumer.name(semantic);
	}

	/**
	 * Protect a net track whose name is a physical name, claiming it for this generation.
	 *
	 * Resolves once the track's info is committed, at once for an accepted track, so every
	 * write is checked against its timescale before an identity is spent. Rejects with
	 * {@link Failure} `identity` if the track name is not a physical name, `reuse` if this
	 * generation already produced it, or the net error if the track closes before accepting.
	 */
	async produce(track: NetTrack.Producer): Promise<Track.Producer> {
		const name = parse(track.name);
		if (this.#claims.has(name)) throw new Failure("reuse");
		this.#claims.add(name);
		let info: NetTrack.Info;
		try {
			info = await track.info();
		} catch (err) {
			// Nothing was produced or spent, so a later track under this name may still claim it.
			this.#claims.delete(name);
			throw err;
		}
		return Track.Producer.make(track, info.timescale ?? undefined, keys(this.credential, this.epoch, name));
	}
}
