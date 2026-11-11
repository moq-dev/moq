import * as Json from "@moq/json";
import type { Broadcast, Timed } from "@moq/net";
import { race } from "@moq/signals";
import type * as z from "zod/mini";
import {
	type BucketFrame,
	BucketFrameSchema,
	type PrefixFrame,
	PrefixFrameSchema,
	type PresenceFrame,
	PresenceFrameSchema,
	type TotalsFrame,
	TotalsFrameSchema,
} from "./frame.ts";
import { parseBucketTrack, prefixTrack, presenceTrack, totalsTrack } from "./track.ts";

// The readers' constructors are private; these let `Consumer` build them.
let makeReader: <T>(broadcast: Broadcast.Consumer, name: string, schema: z.ZodMiniType<T>) => Reader<T>;
let makeBucketReader: (broadcast: Broadcast.Consumer, bucket: string) => BucketReader;

/**
 * Reads one node's stats broadcast (a `<prefix>/node/<node>` announce), yielding typed frames per
 * track. A presence or prefix track is produced only while subscribed.
 */
export class Consumer {
	#broadcast: Broadcast.Consumer;

	/** Wrap a stats broadcast, such as `origin.follow(path).active`'s value. */
	constructor(broadcast: Broadcast.Consumer) {
		this.#broadcast = broadcast;
	}

	/** Subscribe to the totals track. */
	totals(): Reader<TotalsFrame> {
		return makeReader(this.#broadcast, totalsTrack(), TotalsFrameSchema);
	}

	/** Subscribe to the presence track for stats label `label`. */
	presence(label: string): Reader<PresenceFrame> {
		return makeReader(this.#broadcast, presenceTrack(label), PresenceFrameSchema);
	}

	/** Subscribe to the prefix track for broadcast prefix `path`. */
	prefix(path: string): Reader<PrefixFrame> {
		return makeReader(this.#broadcast, prefixTrack(path), PrefixFrameSchema);
	}

	/** Read bucket `bucket`, following the track listing to each new counter run. */
	bucket(bucket: string): BucketReader {
		return makeBucketReader(this.#broadcast, bucket);
	}
}

/**
 * A typed reader over one snapshot track, yielding only the latest frame. Frames a slow reader
 * missed are collapsed, which is safe because the counters are cumulative.
 */
export class Reader<T> {
	#inner: Json.Snapshot.Consumer<T>;

	private constructor(broadcast: Broadcast.Consumer, name: string, schema: z.ZodMiniType<T>) {
		this.#inner = new Json.Snapshot.Consumer({ track: broadcast.track(name), schema });
	}

	static {
		makeReader = (broadcast, name, schema) => new Reader(broadcast, name, schema);
	}

	/** The next frame, or `undefined` once the track ends. */
	async next(): Promise<T | undefined> {
		return (await this.#inner.latest())?.value;
	}

	/** End the subscription. Idempotent. */
	close(): void {
		this.#inner.close();
	}
}

/** A bucket's newest frame, or the end of its run with no newer run listed yet. */
export type BucketUpdate = { kind: "frame"; frame: BucketFrame } | { kind: "ended" };

interface Run {
	name: string;
	reader: Json.Snapshot.Consumer<BucketFrame>;
	pending?: Promise<Timed<BucketFrame> | undefined>;
}

/**
 * Reads one bucket across its counter runs. Each run is its own `bucket.<epoch>.json` track, found
 * through the broadcast's track listing, and every counter in a new run starts from zero: a frame
 * whose `epoch` differs from the last one's starts a fresh series.
 */
export class BucketReader {
	#broadcast: Broadcast.Consumer;
	#bucket: string;
	#tracks: Broadcast.Tracks;
	// Reads that lost a race stay pending and are taken up by the next call, so nothing is dropped.
	#listing?: Promise<Broadcast.TracksEvent | undefined>;
	#run?: Run;
	#closed = false;

	private constructor(broadcast: Broadcast.Consumer, bucket: string) {
		this.#broadcast = broadcast;
		this.#bucket = bucket;
		this.#tracks = broadcast.tracks();
	}

	static {
		makeBucketReader = (broadcast, bucket) => new BucketReader(broadcast, bucket);
	}

	/**
	 * The current run's newest frame, `ended` once that run ends with no newer one listed, or
	 * `undefined` once the broadcast ends. Throws when a run's track fails, such as a frame too
	 * large to cache; the next call waits for a newer run.
	 */
	async next(): Promise<BucketUpdate | undefined> {
		for (;;) {
			if (this.#closed) return undefined;
			this.#listing ??= this.#tracks.next();
			const run = this.#run;
			if (run) run.pending ??= run.reader.latest();

			const listing = this.#listing.then((event) => ({ event }) as const);
			let result: Awaited<typeof listing> | { frame: Timed<BucketFrame> | undefined };
			try {
				result = await (run?.pending ? race([listing, run.pending.then((frame) => ({ frame }))]) : listing);
			} catch (err) {
				// The listing never rejects, so the run failed.
				if (run) this.#end(run);
				throw err;
			}
			if (this.#closed) return undefined;

			if ("frame" in result) {
				if (!run || this.#run !== run) continue;
				run.pending = undefined;
				if (result.frame) return { kind: "frame", frame: result.frame.value };
				// The run's track ended.
				this.#end(run);
				return { kind: "ended" };
			}

			this.#listing = undefined;
			const event = result.event;
			if (!event) {
				this.close();
				return undefined;
			}
			if (event.kind === "added" && parseBucketTrack(event.name)?.bucket === this.#bucket) {
				// A newer run supersedes the current one.
				if (this.#run) this.#end(this.#run);
				const track = this.#broadcast.track(event.name);
				this.#run = {
					name: event.name,
					reader: new Json.Snapshot.Consumer({ track, schema: BucketFrameSchema }),
				};
			} else if ((event.kind === "removed" || event.kind === "finished") && event.name === this.#run?.name) {
				this.#end(this.#run);
				return { kind: "ended" };
			}
		}
	}

	#end(run: Run): void {
		run.reader.close();
		if (this.#run === run) this.#run = undefined;
	}

	/** Stop reading. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#tracks.close();
		if (this.#run) this.#end(this.#run);
	}
}
