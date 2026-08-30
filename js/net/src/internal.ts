/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Producer, Request, Subscriber } from "./track.ts";

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
	/** Re-resolve a subscriber's publisher-selected start, preserving an announced floor. */
	positionSubscriber: (subscriber: Subscriber, announcedStart?: number) => void;
} = {
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
	positionSubscriber: () => {
		throw new Error("track.ts not loaded");
	},
};
