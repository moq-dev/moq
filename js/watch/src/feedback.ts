import { Instant, type Milli, Nano, now, wall } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";

/** A cumulative counter a viewer reports for each rendition it read. */
export type Counter = "frames" | "bytes" | "decoded" | "late" | "errors" | "stalls" | "stalled" | "underruns";

/**
 * What a viewer received and played of one rendition, in the shape its feedback track reports.
 * Counters are cumulative and a missing one means zero; `stalled` is in milliseconds.
 */
export type Received = Partial<Record<Counter, number>> & {
	/** The newest received media timestamp in milliseconds, and its arrival in milliseconds since the MoQ epoch. */
	arrival?: { timestamp: number; wall: number };
};

/** One rendition a decoder read: its ID in the catalog of the broadcast at `path`. */
export interface Key {
	/** The catalog broadcast, which a feedback report is published for. */
	path: Moq.Path.Valid;
	/** The rendition's ID in that catalog, even when its media comes from another broadcast. */
	id: string;
}

// The most catalog broadcasts a tally keeps counters for. A switch overlaps the outgoing and
// incoming one, so a few cover it without holding every broadcast ever watched.
const MAX_BROADCASTS = 4;

type Entry = { received: Received; stalling?: Instant };

/**
 * Cumulative counters per rendition, for each catalog broadcast a decoder read. The decoder counts
 * into it as frames arrive and play, and the feedback reporter reads it about once a second.
 */
export class Tally {
	#broadcasts = new Map<Moq.Path.Valid, Map<string, Entry>>();

	#entry(key: Key): Entry {
		let renditions = this.#broadcasts.get(key.path);
		if (!renditions) {
			renditions = new Map();
			this.#broadcasts.set(key.path, renditions);
			for (const path of this.#broadcasts.keys()) {
				if (this.#broadcasts.size <= MAX_BROADCASTS) break;
				this.#broadcasts.delete(path);
			}
		}
		let entry = renditions.get(key.id);
		if (!entry) {
			entry = { received: {} };
			renditions.set(key.id, entry);
		}
		return entry;
	}

	/** Add `counts` to the rendition's counters. */
	count(key: Key, counts: Partial<Record<Counter, number>>): void {
		const received = this.#entry(key).received;
		for (const [counter, n] of Object.entries(counts) as [Counter, number][]) {
			if (n !== 0) received[counter] = (received[counter] ?? 0) + n;
		}
	}

	/** Record a received frame at media `timestamp`, arriving now. */
	arrive(key: Key, timestamp: Milli): void {
		const at = Math.floor(wall() - Catalog.MOQ_EPOCH_UNIX_MILLIS);
		// A clock before the MoQ epoch has no wire encoding, so such an arrival is never recorded.
		if (at < 0 || timestamp < 0) return;
		this.#entry(key).received.arrival = { timestamp: Math.floor(timestamp), wall: at };
	}

	/**
	 * Count a stall that starts now, returning the function that ends it and reports how long it
	 * lasted in milliseconds. A report taken meanwhile includes the time stalled so far.
	 */
	stall(key: Key): () => number {
		const entry = this.#entry(key);
		const start = now();
		entry.received.stalls = (entry.received.stalls ?? 0) + 1;
		entry.stalling = start;
		return () => {
			const duration = elapsed(start);
			if (entry.stalling === start) entry.stalling = undefined;
			entry.received.stalled = (entry.received.stalled ?? 0) + duration;
			return duration;
		};
	}

	/** What was received of each rendition of the broadcast at `path`, keyed by rendition ID. */
	report(path: Moq.Path.Valid): Record<string, Received> {
		const report: Record<string, Received> = {};
		for (const [id, entry] of this.#broadcasts.get(path) ?? []) {
			const received = { ...entry.received };
			if (entry.stalling !== undefined) received.stalled = (received.stalled ?? 0) + elapsed(entry.stalling);
			if (received.stalled !== undefined) received.stalled = Math.floor(received.stalled);
			if (received.arrival) received.arrival = { ...received.arrival };
			report[id] = received;
		}
		return report;
	}
}

// Milliseconds since `start`.
function elapsed(start: Instant): number {
	return Nano.toMilli(Instant.durationSince(now(), start));
}
