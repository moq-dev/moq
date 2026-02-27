import type { AnnounceInterest } from "./announce.ts";
import type { Group } from "./group.ts";
import type { SessionClient } from "./session.ts";
import type { Subscribe } from "./subscribe.ts";

export type StreamBi = SessionClient | AnnounceInterest | Subscribe;
export type StreamUni = Group;

export const StreamId = {
	Session: 0,
	Announce: 1,
	Subscribe: 2,
	Fetch: 3,
	Probe: 4,
	ClientCompat: 0x20,
	ServerCompat: 0x21,
} as const;
