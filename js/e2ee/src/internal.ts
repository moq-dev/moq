// Package-private state the public modules share without exporting it.
import { Error as NetError, type Time } from "@moq/net";
import type { TrackKey } from "./key.ts";
import type { Pump } from "./pump.ts";

/** State one track producer shares with its groups. */
export interface Writer {
	pump: Pump;
	key: TrackKey;
	// Whether the net track is timed, once its info has resolved.
	timed?: boolean;
	// Set by any close: no new writes.
	closed: boolean;
	aborted?: Error;
}

/** Refuse a timestamp whose presence disagrees with the track's committed timescale. */
export function checkTimed(writer: Writer, timestamp: Time.Timestamp | undefined): void {
	if (writer.timed !== undefined && writer.timed !== (timestamp !== undefined)) {
		throw new NetError.TimestampMismatch();
	}
}
