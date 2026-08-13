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

/** Hooks assigned in static blocks by the owning class. */
export const hooks: {
	/** Mint a track {@link Request}; assigned by `track.ts`. */
	makeRequest: (name: string, producer: Producer) => Request;
	/** Pop one immediately readable group without waiting; assigned by `track.ts`. */
	tryRecvGroup: (subscriber: Subscriber) => GroupConsumer | Error | null | undefined;
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
