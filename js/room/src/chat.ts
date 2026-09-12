/**
 * Chat over an uncompressed JSON window on `chat`, retaining ten seconds of messages.
 * Sender identity comes from the broadcast, not the payload.
 * @module
 */

import * as Json from "@moq/json";
import type { Broadcast, Track } from "@moq/net";
import { Effect, Signal } from "@moq/signals";

/** Name of the track carrying the chat window. */
export const TRACK = "chat";
/** Delivery priority, below audio and video. */
export const PRIORITY = 10;
/** Milliseconds a published message stays in the window. */
export const HISTORY = 10_000;
/** A message entering, leaving, or missed from the window. */
export type Event = Json.Window.Event<string>;

/** Track settings for the latest chat window. */
export function info(): Pick<Track.Info, "priority" | "ordered"> {
	return { priority: PRIORITY, ordered: false };
}

/** Publishes chat messages and retires them after ten seconds, including while idle. */
export class Publisher {
	#producer: Json.Window.Producer<string>;
	#expires = new Signal<number[]>([]);
	#signals = new Effect();

	/** Create the chat track on a broadcast. */
	static create(broadcast: Broadcast.Producer): Publisher {
		return new Publisher(broadcast.createTrack(TRACK, info()));
	}

	/** Publish a chat window over an existing track. */
	constructor(track: Track.Producer) {
		// Every edit restates the retained window, so a late reader never replays expired records.
		this.#producer = new Json.Window.Producer(track, { opRatio: 0 });
		this.#signals.run((effect) => {
			const next = effect.get(this.#expires)[0];
			if (next === undefined) return;
			effect.timer(() => this.expire(), Math.max(0, next - performance.now()));
		});
	}

	/** Append nonempty text, first retiring messages whose history has elapsed. */
	send(text: string): void {
		if (!text) return;
		this.expire();
		this.#producer.push(text);
		this.#expires.update((expires) => [...expires, performance.now() + HISTORY]);
	}

	/** Retire elapsed messages now; the publisher also schedules this automatically. */
	expire(): void {
		const now = performance.now();
		const expires = this.#expires.peek();
		let count = 0;
		while (count < expires.length && expires[count] <= now) count++;
		if (!count) return;
		this.#producer.pop(count);
		this.#expires.set(expires.slice(count));
	}

	/** Finish the track and cancel expiry timers. */
	finish(): void {
		this.#signals.close();
		this.#producer.finish();
	}
}

/** Reads changes to a participant's retained chat window. */
export class Subscriber {
	#track: Track.Subscriber;
	#consumer: Json.Window.Consumer<unknown>;

	/** Subscribe to the newest retained window on a broadcast. */
	static subscribe(broadcast: Broadcast.Consumer): Subscriber {
		return new Subscriber(broadcast.track(TRACK).subscribe({ ordered: false }));
	}

	/** Read window changes from an existing subscription. */
	constructor(track: Track.Subscriber) {
		const latest = track.latest();
		if (latest !== undefined) track.startAt(latest);
		this.#track = track;
		this.#consumer = new Json.Window.Consumer(track);
	}

	/** Return the next window change, or undefined on clean completion; failures throw. */
	async recv(): Promise<Event | undefined> {
		const event = await this.#consumer.next();
		if (!event) return undefined;
		if ("push" in event) {
			if (typeof event.push.value !== "string") throw new Error("chat record must be a string");
			return { push: { index: event.push.index, value: event.push.value } };
		}
		return event;
	}

	/** Cancel the subscription and release buffered records. */
	close(): void {
		this.#track.close();
	}
}
