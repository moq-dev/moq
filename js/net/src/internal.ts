/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Dispose, Getter } from "@moq/signals";
import type { Frame, Consumer as GroupConsumer } from "./group.ts";
import type { Timestamp } from "./time.ts";
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

/** Where a group cursor sits while measuring drift against the live edge. */
export interface GroupPosition {
	/** Presentation timestamp of the next or in-flight frame. */
	presentation?: Timestamp;
	/** Monotonic arrival time of the next or in-flight frame. */
	activity?: number;
}

/** A package-internal frame read paired with its guarded cursor position. */
export interface ReadGroupFrame {
	/** Frame sequence within the group. */
	sequence: number;
	/** Frame returned to the publisher. */
	frame: Frame;
	/** Cursor position held while the frame is in flight. */
	position: GroupPosition;
	/** Mark the frame delivered or deliberately skipped by the wire publisher. */
	complete(): void;
}

/** The next group or datagram sequence shared by dynamic producers of one broadcast track. */
export interface TrackSequence {
	next: number;
}

/** Per-track sequence namespaces owned by one broadcast generation. */
export type TrackSequences = Map<string, TrackSequence>;

/** Inputs for creating a package-internal track request. */
export interface TrackRequestOptions {
	/** The requested track name. */
	name: string;
	/** The producer that will serve the request. */
	producer: Producer;
	/** Sequence namespaces shared by the broadcast generation. */
	sequences: TrackSequences;
}

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (options: TrackRequestOptions) => Request;
	/**
	 * Take the next group the subscriber's cursor allows, without waiting; assigned by `track.ts`.
	 *
	 * Synchronous so a caller can pop a group and act on it in the same turn. Park on
	 * {@link groupChanged} when it reports `idle` or `boundary`.
	 */
	tryRecvGroup: (subscriber: Subscriber) => Recv;
	/** Wake once a subscriber's group cursor may read differently; assigned by `track.ts`. */
	groupChanged: (subscriber: Subscriber, fn: () => void) => Dispose;
	/**
	 * Exempt a subscriber from live-delivery policy for a one-shot FETCH scan: it names one
	 * old group explicitly, so it is neither late against the live edge nor bound by the start
	 * a live subscription resolves to.
	 */
	exemptFetch: (subscriber: Subscriber) => void;
	/** Return a group's first timestamp, retained even after its first frame is read. */
	groupTimestamp: (group: GroupConsumer) => Timestamp | undefined;
	/** Keep applying a subscription's drift policy after it hands a group out. */
	expireGroup: (
		group: GroupConsumer,
		expiry: { expired: (at: GroupPosition) => boolean; changed: readonly Getter<unknown>[] },
	) => void;
	/** Stop an in-flight group operation if the handed-out group expires. */
	guardGroup: <T>(group: GroupConsumer, operation: Promise<T>, at?: GroupPosition) => Promise<T>;
	/** Read a frame with the cursor position needed to guard its wire write. */
	readGroupFrame: (group: GroupConsumer) => Promise<ReadGroupFrame | undefined>;
	/** Make an evicted mirror terminal while its track timeline still contains it. */
	evictGroup: (group: GroupConsumer) => void;
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
	exemptFetch: () => {
		throw new Error("track.ts not loaded");
	},
	groupTimestamp: () => {
		throw new Error("group.ts not loaded");
	},
	expireGroup: () => {
		throw new Error("group.ts not loaded");
	},
	guardGroup: () => {
		throw new Error("group.ts not loaded");
	},
	readGroupFrame: () => {
		throw new Error("group.ts not loaded");
	},
	evictGroup: () => {
		throw new Error("group.ts not loaded");
	},
};
