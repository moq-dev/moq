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
	// The net track's committed timescale, or undefined when untimed.
	timescale?: Time.Timescale;
	// Set by release or abort: no new writes, including on existing groups.
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

/** Refuse a timestamp the track could not encode: wrong presence, or out of range at its timescale. */
export function checkTimestamp(writer: Writer, timestamp: Time.Timestamp | undefined): void {
	if ((writer.timescale !== undefined) !== (timestamp !== undefined)) {
		throw new NetError.TimestampMismatch();
	}
	// Throws a RangeError for a conversion that would only fail at encode, after the identity is spent.
	if (timestamp && writer.timescale !== undefined) timestamp.as(writer.timescale);
}
