import type { AnnounceInterest } from "./announce.ts";
import type { Goaway } from "./goaway.ts";
import type { Group } from "./group.ts";
import type { SessionClient } from "./session.ts";
import type { Subscribe } from "./subscribe.ts";

export type StreamBi = SessionClient | AnnounceInterest | Subscribe | Goaway;
export type StreamUni = Group;

export const StreamId = {
	Session: 0,
	Announce: 1,
	Subscribe: 2,
	Fetch: 3,
	Probe: 4,
	Goaway: 5,
	/// Queries a track's immutable publisher properties via TRACK/TRACK_INFO (moq-lite-05+).
	Track: 6,
	ClientCompat: 0x20,
	ServerCompat: 0x21,
} as const;

/// Unidirectional (data) stream types.
export const DataId = {
	Group: 0,
	/// Carries a single SETUP message (moq-lite-05+).
	Setup: 1,
} as const;
