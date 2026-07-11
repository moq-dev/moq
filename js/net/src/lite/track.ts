import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

// The Track Stream (0x6) is draft-05+ only.
function guardTrack(version: Version) {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			throw new Error("track stream not supported for this version");
		default:
			break;
	}
}

/**
 * TRACK request: the first (and only) subscriber message on a Track Stream (0x6).
 * Asks for a track's immutable publisher properties without subscribing or fetching.
 */
export class Track {
	broadcast: Path.Valid;
	track: string;

	constructor(broadcast: Path.Valid, track: string) {
		this.broadcast = broadcast;
		this.track = track;
	}

	async #encode(w: Writer) {
		await w.string(Path.encode(this.broadcast));
		await w.string(this.track);
	}

	static async #decode(r: Reader): Promise<Track> {
		const broadcast = Path.decode(await r.string());
		const track = await r.string();
		return new Track(broadcast, track);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTrack(version);
		return Message.encode(w, (w) => this.#encode(w));
	}

	static async decode(r: Reader, version: Version): Promise<Track> {
		guardTrack(version);
		return Message.decode(r, (r) => Track.#decode(r));
	}
}

/**
 * TRACK_INFO reply: the publisher's sole message on a Track Stream, carrying the
 * track's immutable properties. Fetched once and reused across every SUBSCRIBE and
 * FETCH for the track.
 */
export class TrackInfo {
	priority: number;
	ordered: boolean;
	/**
	 * Publisher Max Latency: an upper bound (milliseconds) on how long the publisher
	 * caches a non-latest group past the arrival of a newer one.
	 */
	cache: number;
	/**
	 * Per-frame timestamp scale (units per second). Mandatory on Lite05: a real
	 * (non-zero) scale, and every frame on the wire is prefixed with a zigzag-delta
	 * timestamp at this scale.
	 */
	timescale: number;

	constructor({
		priority = 0,
		ordered = true,
		cache = 0,
		timescale = 0,
	}: {
		priority?: number;
		ordered?: boolean;
		cache?: number;
		timescale?: number;
	}) {
		this.priority = priority;
		this.ordered = ordered;
		this.cache = cache;
		this.timescale = timescale;
	}

	async #encode(w: Writer) {
		await w.u8(this.priority);
		await w.bool(this.ordered);
		await w.u53(this.cache);
		await w.u53(this.timescale);
	}

	static async #decode(r: Reader): Promise<TrackInfo> {
		const priority = await r.u8();
		const ordered = await r.bool();
		const cache = await r.u53();
		const timescale = await r.u53();
		// Mandatory on Lite05: a zero scale is invalid (mirrors Rust's Timescale::new rejection),
		// and would otherwise throw later when wrapped in Timescale().
		if (timescale === 0) throw new Error("track timescale must be non-zero");
		return new TrackInfo({ priority, ordered, cache, timescale });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTrack(version);
		// Reject a zero timescale on encode too, so an invalid TrackInfo fails fast on
		// the sender rather than only at the peer's decoder.
		if (this.timescale === 0) throw new Error("track timescale must be non-zero");
		return Message.encode(w, (w) => this.#encode(w));
	}

	static async decode(r: Reader, version: Version): Promise<TrackInfo> {
		guardTrack(version);
		return Message.decode(r, (r) => TrackInfo.#decode(r));
	}
}
