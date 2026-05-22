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
	ClientCompat: 0x20,
	ServerCompat: 0x21,
} as const;
