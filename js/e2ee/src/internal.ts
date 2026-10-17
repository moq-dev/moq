// Package-private state the public modules share without exporting it.
import { Error as NetError, type Time } from "@moq/net";
import type { TrackKey } from "./key.ts";
import type { Pump } from "./pump.ts";
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
	// Whether the net track is timed, from its committed info.
	timed: boolean;
	// Set by any close: no new writes.
	closed: boolean;
	aborted?: Error;
}

/** State one track consumer shares with its groups. */
export interface Reader {
	// One open in flight across the track's groups and datagrams; net reads wait outside it.
	pump: Pump;
	key: TrackKey;
	terminal: Terminal;
}

/** Refuse a timestamp whose presence disagrees with the track's committed timescale. */
export function checkTimed(writer: Writer, timestamp: Time.Timestamp | undefined): void {
	if (writer.timed !== (timestamp !== undefined)) {
		throw new NetError.TimestampMismatch();
	}
}
