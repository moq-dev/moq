/**
 * Package-internal constructor hooks. Classes keep their constructors private so consumers
 * can't mint detached handles; sibling modules create instances through these hooks instead.
 * Not exported from the package entrypoint.
 *
 * @module
 */
import type { Producer, Request } from "./track.ts";

/** The next group or datagram sequence shared by dynamic producers of one broadcast track. */
export interface TrackSequence {
	next: number;
}

/** Per-track sequence namespaces owned by one broadcast generation. */
export type TrackSequences = Map<string, TrackSequence>;

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (name: string, producer: Producer, sequences: TrackSequences) => Request;
} = {
	makeRequest: () => {
		throw new Error("track.ts not loaded");
	},
};
