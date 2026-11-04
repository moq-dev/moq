// Package-private state the public modules share without exporting it.
import type { Track as NetTrack } from "@moq/net";
import type { Pump } from "../pump.ts";
import type { TrackKey } from "./key.ts";
import type { Terminal } from "./terminal.ts";

/** A track's grouped-frame and datagram keys. */
export interface Keys {
	group: TrackKey;
	datagram: TrackKey;
}

/** State one track producer shares with its groups. */
export interface Writer {
	pump: Pump;
	key: TrackKey;
	// The net track's committed info, which every timestamp is rescaled to before encryption.
	track: Pick<NetTrack.Info, "timescale">;
	// Set by release or abort: no new writes, including on existing groups.
	closed: boolean;
	aborted?: Error;
}

/** State one track consumer shares with its groups. */
export interface Reader {
	// One open in flight across the stream track's groups; net reads wait outside it.
	pump: Pump;
	key: TrackKey;
	terminal: Terminal;
}
