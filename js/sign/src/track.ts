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

/**
 * Producer for one signed track.
 *
 * Every write on the track and its groups runs through one serial pump, so net writes land in
 * call order.
 */
export class Producer {
	/** The track name. */
	readonly name: string;

	#inner: NetTrack.Producer;
	#writer: Group.Writer;
	#finished = false;

	private constructor(inner: NetTrack.Producer, key: SecretKey, scope: Scope, timescale?: Time.Timescale) {
		this.#inner = inner;
		this.name = inner.name;
		this.#writer = { pump: new Pump(), key, scope, timescale, closed: false };
	}

	/** Sign `inner`, whose info declared `timescale` (undefined when untimed). @internal */
	static make(inner: NetTrack.Producer, key: SecretKey, scope: Scope, timescale?: Time.Timescale): Producer {
		return new Producer(inner, key, scope, timescale);
	}

	/** The track's final outcome, once its declared end is reached or it aborts. */
	get closed(): NetTrack.Producer["closed"] {
		return this.#inner.closed;
	}

	/** Start a group at the next sequence. */
	appendGroup(): Group.Producer {
		this.#writable();
		return Group.Producer.make(this.#inner.appendGroup(), this.#writer);
	}

	/** Start a group at an explicit sequence; throws the net error if the track already holds it. */
	createGroup(sequence: number): Group.Producer {
		this.#writable();
		return Group.Producer.make(this.#inner.createGroup(sequence), this.#writer);
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
