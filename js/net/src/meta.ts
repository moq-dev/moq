/**
 * Metadata subscriptions: a track's group and frame progress without its payload, mirroring the
 * Rust `meta` module.
 *
 * A metadata subscription is a subscription of its own beside any payload subscription to the
 * same track, with its mode fixed for its life. Where the hop carries the mode (moq-lite-07), it
 * travels upstream as a metadata subscription and no payload crosses that hop for it. Over an
 * older version it rides the track's payload subscription and drops the payload, so it never
 * downloads the payload twice.
 *
 * A reader that wants the payload after all opens a payload subscription from the group it reached
 * and reads on from its first missing frame (`Group.Consumer.readFrameIndex` with `from`): nothing
 * changes mode in place.
 *
 * @module
 */

import type * as frame from "./frame.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import { hooks } from "./internal.ts";
import { readSize } from "./payload.ts";
import type * as track from "./track.ts";

let makeConsumer: (track: track.Consumer) => Consumer;
let makeSubscriber: (inner: track.Subscriber) => Subscriber;
let makeGroup: (inner: GroupConsumer) => Group;

/** A metadata handle to `track`, which must resolve the copy a metadata reader takes. @internal */
export function consumerOf(track: track.Consumer): Consumer {
	return makeConsumer(track);
}

/**
 * A lazy handle to a track's metadata, from `Broadcast.Consumer.meta`. Like `Track.Consumer`,
 * holding it requests nothing: only a {@link subscribe} made from it is demand.
 *
 * @public
 */
export class Consumer {
	/** The track name. */
	readonly name: string;

	#track: track.Consumer;

	private constructor(track: track.Consumer) {
		this.name = track.name;
		this.#track = track;
	}

	static {
		makeConsumer = (track) => new Consumer(track);
	}

	/**
	 * Open a live metadata subscription, delivering the groups `options` covers as
	 * `Track.Consumer.subscribe` does, each frame as its header alone.
	 */
	subscribe(options?: track.Subscription): Subscriber {
		return makeSubscriber(this.#track.subscribe(options));
	}
}

/**
 * A live metadata subscription: a `Track.Subscriber` whose groups hold headers alone.
 *
 * @public
 */
export class Subscriber {
	#inner: track.Subscriber;

	private constructor(inner: track.Subscriber) {
		this.#inner = inner;
	}

	static {
		makeSubscriber = (inner) => new Subscriber(inner);
	}

	/** The track's publisher properties, once the subscription is established. */
	info(): Promise<track.Info> {
		return this.#inner.info();
	}

	/** Resolve with the track's exclusive end once it is known, as `Track.Subscriber.finished` does. */
	finished(): Promise<track.Position> {
		return this.#inner.finished();
	}

	/** Receive the next group, as `Track.Subscriber.recvGroup` does. */
	async recvGroup(): Promise<Group | undefined> {
		const group = await this.#inner.recvGroup();
		return group ? makeGroup(group) : undefined;
	}

	/** Replace this subscription's options; see `Track.Subscriber.update`. */
	update(options: track.Subscription): void {
		this.#inner.update(options);
	}

	/** Close the subscription (optionally with an error). Idempotent. */
	close(abort?: Error): void {
		this.#inner.close(abort);
	}
}

/**
 * One group of a metadata subscription, read header by header.
 *
 * @public
 */
export class Group {
	/** Where this group starts, in its track's timescale: its key on the track. */
	readonly start: bigint;

	#inner: GroupConsumer;
	#index = 0;

	private constructor(inner: GroupConsumer) {
		this.start = inner.start;
		this.#inner = inner;
	}

	static {
		makeGroup = (inner) => new Group(inner);
	}

	/** The index of the next frame this reader returns. */
	get index(): number {
		return this.#index;
	}

	/**
	 * The next frame's header: its timestamp and declared plain size, without its payload, or
	 * undefined once the group ends. A real empty frame reads as size 0.
	 */
	async nextHeader(): Promise<frame.Info | undefined> {
		// A stored read carries each frame's plain size beside it, so it never inflates a frame.
		const read = await hooks.readGroupFrame(this.#inner, true, 0, true);
		if (!read) return undefined;
		this.#index = read.index + 1;
		return { size: read.inflated ?? readSize(read), timestamp: read.frame.timestamp };
	}

	/** Release this reader, like dropping a Rust consumer. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}
