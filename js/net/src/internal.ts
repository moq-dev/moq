/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Dispose } from "@moq/signals";
import type { Consumer as GroupConsumer } from "./group.ts";
import type { Producer, Request, Subscriber } from "./track.ts";

/**
 * What a non-blocking group read found, which is everything the caller needs to decide what
 * to do next: no second look at the track's closed state, and no ordering rule to get wrong.
 */
export type Recv =
	/** A group to serve. It has already left the buffer, so dropping this is dropping the group. */
	| { kind: "group"; group: GroupConsumer }
	/** Nothing readable, but the track is live and may produce more. */
	| { kind: "idle" }
	/** The producer finished, yet groups above the cap are still held: raising it releases them. */
	| { kind: "boundary" }
	/** The producer finished and the buffer is drained. Nothing can follow. */
	| { kind: "done" }
	/** The track aborted. */
	| { kind: "error"; error: Error };

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (name: string, producer: Producer) => Request;
	/**
	 * Take the next group the subscriber's cursor allows, without waiting; assigned by `track.ts`.
	 *
	 * Synchronous so a caller can pop a group and act on it in the same turn. Park on
	 * {@link groupChanged} when it reports `idle` or `boundary`.
	 */
	tryRecvGroup: (subscriber: Subscriber) => Recv;
	/** Wake once a subscriber's group cursor may read differently; assigned by `track.ts`. */
	groupChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
} = {
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
	tryRecvGroup: () => {
		throw new Error("track.ts not loaded");
	},
	groupChanged: () => {
		throw new Error("track.ts not loaded");
	},
};
