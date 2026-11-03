export const StreamId = {
	Session: 0,
	Announce: 1,
	Subscribe: 2,
	Fetch: 3,
	Probe: 4,
	Goaway: 5,
	Track: 6,
	Auth: 7,
	Tracks: 8,
	ClientCompat: 0x20,
	ServerCompat: 0x21,
} as const;

/**
 * The varint type prefix on a unidirectional data stream.
 *
 * `Group` is the per-group frame stream. `Setup` (lite-05+) carries the single SETUP
 * message advertising this endpoint's capabilities.
 */
export const DataType = {
	Group: 0,
	Setup: 1,
	/** Arbitrary bytes discarded by lite-07+ receivers. */
	Padding: 2,
} as const;

/** A unidirectional data-stream type. See {@link DataType}. */
export type DataType = (typeof DataType)[keyof typeof DataType];
