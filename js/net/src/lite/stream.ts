import type { AnnounceInterest } from "./announce.ts";
import type { Goaway } from "./goaway.ts";
import type { Group } from "./group.ts";
import type { SessionClient } from "./session.ts";
import type { Subscribe } from "./subscribe.ts";
import type { Track } from "./track.ts";

export type StreamBi = SessionClient | AnnounceInterest | Subscribe | Track | Goaway;
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
