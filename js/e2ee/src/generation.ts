/**
 * Producing generations mint an epoch; consuming generations bind a discovered one.
 *
 * The package exports these as types only: {@link Credential.produce} and
 * {@link Credential.consume} are the only way to obtain one, and nothing accepts an epoch for
 * production. Two producers under one epoch would repeat every nonce; see the draft's
 * [epoch rules](https://doc.moq.dev/draft/draft-lcurley-moq-e2ee#epoch).
 *
 * @module
 */
import { Epoch, type Track as NetTrack } from "@moq/net";
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

/**
 * A consume-only generation bound to a discovered epoch.
 *
 * Every subscription through it shares each key's AEAD budget, so resubscribing cannot reset
 * it. The budget lives as long as the generation; binding the epoch again with
 * {@link Credential.consume} starts another.
 */
export class Consumer {
	/** The credential this generation derives from. */
	readonly credential: Credential;
	/** The epoch this generation is scoped to. */
	readonly epoch: Epoch.Valid;

	// Never evicted: re-deriving a key would reset its budget. Bounded by the semantic names
	// the application subscribes to, for as long as it holds the generation.
	#keys = new Map<Name, Keys>();

	/** @internal Throws {@link Failure} `identity` if `epoch` is not canonical. */
	constructor(credential: Credential, epoch: Epoch.Valid) {
		try {
			Epoch.parse(epoch);
		} catch {
			throw new Failure("identity");
		}
		this.credential = credential;
		this.epoch = epoch;
	}

	/** The opaque physical track name for a semantic track name; rejects with {@link Failure} `identity` past 65535 bytes. */
	async name(semantic: string): Promise<Name> {
		const { context, kid } = this.credential;
		return encode(await expand(this.credential, nameInfo(context, this.epoch, kid, semantic)));
	}

	/**
	 * Open a net subscription whose track name is a physical name; throws {@link Failure} `identity` if it is not one.
	 *
	 * @internal Applications subscribe through `Broadcast.Consumer`.
	 */
	consume(track: NetTrack.Subscriber): Track.Consumer {
		const name = parse(track.name);
		let shared = this.#keys.get(name);
		if (!shared) {
			shared = keys(this.credential, this.epoch, name);
			this.#keys.set(name, shared);
		}
		return Track.Consumer.make(track, shared);
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

	/** @internal Mints the epoch itself, so no caller can choose one. */
	constructor(credential: Credential) {
		this.credential = credential;
		this.epoch = Epoch.mint();
		this.#consumer = new Consumer(credential, this.epoch);
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
	 * A refused track is closed with the failure, so its subscribers see why.
	 *
	 * @internal Applications create tracks through `Broadcast.Producer`.
	 */
	async produce(track: NetTrack.Producer): Promise<Track.Producer> {
		let name: Name;
		try {
			name = parse(track.name);
			if (this.#claims.has(name)) throw new Failure("reuse");
		} catch (err) {
			// Both refusals are Failures.
			track.close(err as Failure);
			throw err;
		}
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
