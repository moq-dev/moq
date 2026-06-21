import type { AnnounceRequest } from "./announce.ts";
import type { Goaway } from "./goaway.ts";
import type { Group } from "./group.ts";
import type { SessionClient } from "./session.ts";
import type { Subscribe } from "./subscribe.ts";
import type { Track } from "./track.ts";

export type StreamBi = SessionClient | AnnounceRequest | Subscribe | Track | Goaway;
export type StreamUni = Group;

export const StreamId = {
	Session: 0,
	Announce: 1,
	Subscribe: 2,
	Fetch: 3,
	Probe: 4,
	Goaway: 5,
	Track: 6,
	ClientCompat: 0x20,
	ServerCompat: 0x21,
} as const;

/**
 * The type prefix on a unidirectional data stream, read/written as a single byte.
 *
 * `Group` is the per-group frame stream. `Setup` (lite-05+) carries the single SETUP
 * message advertising this endpoint's capabilities.
 */
export const DataType = {
	Group: 0,
	Setup: 1,
} as const;

/** A unidirectional data-stream type. See {@link DataType}. */
export type DataType = (typeof DataType)[keyof typeof DataType];
