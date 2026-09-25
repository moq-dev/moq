/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Dispose, Getter } from "@moq/signals";
import type { Producer as BroadcastProducer } from "./broadcast.ts";
import type { Frame, Consumer as GroupConsumer } from "./group.ts";
import type { Route } from "./hop.ts";
import * as Path from "./path.ts";
import type { Timestamp } from "./time.ts";
import type { Groups, Producer, Request, Subscriber } from "./track.ts";

/** Normalize public group bounds into an inclusive start and exclusive end. */
export function groupBounds(groups: Groups = {}): { start: number; end?: number } {
	const bound = (value: Groups["start"] | undefined, start: boolean): number | undefined => {
		if (value === undefined) return undefined;
		if ((value.included === undefined) === (value.excluded === undefined)) {
			throw new Error("a group bound must be either included or excluded");
		}
		const sequence = value.included ?? value.excluded;
		if (sequence === undefined || !Number.isSafeInteger(sequence) || sequence < 0) {
			throw new Error("a group bound must be a non-negative safe integer");
		}
		return sequence + (start ? Number(value.excluded !== undefined) : Number(value.included !== undefined));
	};
	return { start: bound(groups.start, true) ?? 0, end: bound(groups.end, false) };
}

/**
 * The announce-interest prefix a scope needs on a prefix-shaped wire: its literal head.
 * The peer echoes every suffix beneath it, and the caller filters what arrives.
 */
export function scopeHead(scope: Path.Pattern): Path.Valid {
	return Path.from(scope.head);
}

/**
 * Whether a segment of `path` below `prefix` starts with `.`, which hides it from announce
 * discovery unless the request opts in. A path at or above the prefix never hides.
 */
export function hiddenBelow(prefix: Path.Valid, path: Path.Valid): boolean {
	const below = Path.stripPrefix(prefix, path);
	return below !== null && Path.parts(below).some((part) => part.startsWith("."));
}

/** Whether the announced prefix's subtree overlaps `scope`. */
export function scopeOverlaps(scope: Path.Pattern, prefix: Path.Valid): boolean {
	return scope.overlaps(Path.Pattern.subtree(prefix));
}

/** What `scope` captures from an exact announced prefix, if it pins every wildcard. */
export function scopeCaptures(scope: Path.Pattern, prefix: Path.Valid): Path.Pattern[] | undefined {
	return scope.captures(Path.Pattern.literal(prefix));
}

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

/** A package-internal frame read the wire publisher completes once written. */
export interface ReadGroupFrame {
	/** Frame sequence within the group. */
	sequence: number;
	/** Frame returned to the publisher. */
	frame: Frame;
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
	/** Requests not yet accepted or rejected by the publisher. */
	pending: Set<Request>;
}

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (options: TrackRequestOptions) => Request;
	/** Access the existing producer while a request awaits immutable wire metadata. */
	pendingTrackProducer: (request: Request) => Producer;
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
	/**
	 * Replace a serving cursor. An omitted start keeps the current floor; a provided start
	 * can lower it. Wire publishers apply SUBSCRIBE_UPDATE here rather than through
	 * `setGroups`, which never rewinds.
	 */
	replaceGroups: (subscriber: Subscriber, groups: Groups) => void;
	/** Return a group's first timestamp, retained even after its first frame is read. */
	groupTimestamp: (group: GroupConsumer) => Timestamp | undefined;
	groupLatest: (group: GroupConsumer) => Timestamp | undefined;
	/** Keep applying a subscription's drift policy after it hands a group out. */
	expireGroup: (
		group: GroupConsumer,
		expiry: { expired: () => boolean; changed: readonly Getter<unknown>[] },
	) => void;
	/** Stop an in-flight group operation if the handed-out group expires. */
	guardGroup: <T>(group: GroupConsumer, operation: Promise<T>) => Promise<T>;
	/** Read a frame the wire publisher completes (or skips) once written. */
	readGroupFrame: (group: GroupConsumer, from?: number) => Promise<ReadGroupFrame | undefined>;
	/** Make an evicted mirror terminal while its track timeline still contains it. */
	evictGroup: (group: GroupConsumer) => void;
	/** Attach the origin advertisement of a created broadcast. */
	attachAnnouncer: (
		producer: BroadcastProducer,
		announcer: { announce(route: Route): void; unannounce(): void },
	) => void;
} = {
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
	pendingTrackProducer: () => {
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
	replaceGroups: () => {
		throw new Error("track.ts not loaded");
	},
	groupTimestamp: () => {
		throw new Error("group.ts not loaded");
	},
	groupLatest: () => {
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
	attachAnnouncer: () => {
		throw new Error("broadcast.ts not loaded");
	},
};
