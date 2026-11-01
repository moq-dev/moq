/**
 * Signing producer and verifying subscriber for one track.
 *
 * @module
 */
import type { Track as NetTrack, Time } from "@moq/net";
import * as Group from "./group.ts";
import type { PublicKey, SecretKey } from "./key.ts";
import { Pump } from "./pump.ts";
import type { Scope } from "./scope.ts";

/** The lowest group sequence an instance has not signed on a track, shared with the track's replacements. @internal */
export interface Sequences {
	next: number;
}

/**
 * Producer for one signed track.
 *
 * Every write on the track and its groups runs through one serial pump, so net writes land in
 * call order.
 *
 * Group sequences only rise under an epoch, even across a replacement of the track, so no two
 * groups are signed at the same coordinates. A relay holding an aborted group and a new group at
 * its sequence could otherwise splice their frames into a group that was never published.
 */
export class Producer {
	/** The track name. */
	readonly name: string;

	#inner: NetTrack.Producer;
	#writer: Group.Writer;
	#sequences: Sequences;
	#finished = false;

	private constructor(
		inner: NetTrack.Producer,
		key: SecretKey,
		scope: Scope,
		sequences: Sequences,
		timescale?: Time.Timescale,
	) {
		this.#inner = inner;
		this.name = inner.name;
		this.#writer = { pump: new Pump(), key, scope, timescale, closed: false };
		this.#sequences = sequences;
	}

	/** Sign `inner`, whose info declared `timescale` (undefined when untimed). @internal */
	static make(
		inner: NetTrack.Producer,
		key: SecretKey,
		scope: Scope,
		sequences: Sequences,
		timescale?: Time.Timescale,
	): Producer {
		return new Producer(inner, key, scope, sequences, timescale);
	}

	/** The track's final outcome, once its declared end is reached or it aborts. */
	get closed(): NetTrack.Producer["closed"] {
		return this.#inner.closed;
	}

	/** Start a group one past the highest sequence this instance signed on the track. */
	appendGroup(): Group.Producer {
		return this.createGroup(this.#sequences.next);
	}

	/**
	 * Start a group at an explicit sequence, above any this instance signed on the track.
	 *
	 * Throws if this instance already signed the sequence or a later one, even in a group since
	 * aborted or evicted, or the net error if the track refuses it.
	 */
	createGroup(sequence: number): Group.Producer {
		this.#writable();
		if (sequence < this.#sequences.next) throw new Error(`duplicate group: sequence=${sequence}`);
		const inner = this.#inner.createGroup(sequence);
		this.#sequences.next = sequence + 1;
		return Group.Producer.make(inner, this.#writer);
	}

	/** Declare the end after queued writes. Existing groups may still be written; new groups are refused. Idempotent. */
	finish(): void {
		if (this.#finished || this.#writer.closed) return;
		this.#finished = true;
		void this.#writer.pump.run(() => {
			if (!this.#writer.aborted) this.#inner.finish();
		});
	}

	/** Abort the track and its open groups at once, rejecting pending writes. Idempotent. */
	abort(error: Error): void {
		if (this.#writer.closed) return;
		this.#writer.closed = true;
		this.#writer.aborted = error;
		this.#inner.abort(error);
	}

	/** Release this producer: drain a declared end, or cancel when nothing finished it. Idempotent. */
	close(): void {
		if (this.#writer.closed) return;
		this.#writer.closed = true;
		if (this.#finished) {
			void this.#writer.pump.run(() => this.#inner.close());
		} else {
			this.#inner.close();
			this.#writer.aborted = this.#inner.closed.peek() ?? undefined;
		}
	}

	#writable(): void {
		if (this.#finished || this.#writer.closed) throw this.#writer.aborted ?? new Error("track is closed");
	}
}

/**
 * Subscriber for one signed track.
 *
 * Each group verifies on its own: a forged frame ends only the group that carried it.
 */
export class Consumer {
	/** The track name. */
	readonly name: string;

	#inner: NetTrack.Subscriber;
	#key: PublicKey;
	#scope: Scope;

	private constructor(inner: NetTrack.Subscriber, key: PublicKey, scope: Scope) {
		this.#inner = inner;
		this.name = inner.name;
		this.#key = key;
		this.#scope = scope;
	}

	/** @internal */
	static make(inner: NetTrack.Subscriber, key: PublicKey, scope: Scope): Consumer {
		return new Consumer(inner, key, scope);
	}

	/** Receive the next group in arrival order, or `undefined` once the track ends. */
	async recvGroup(): Promise<Group.Consumer | undefined> {
		const group = await this.#inner.recvGroup();
		return group && Group.Consumer.make(group, this.#key, this.#scope);
	}

	/** Unsubscribe, releasing track demand. Groups already received end after the frames they hold. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}
