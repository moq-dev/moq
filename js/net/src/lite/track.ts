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
		await w.string(this.broadcast);
		await w.string(this.track);
	}

	static async #decode(r: Reader): Promise<Track> {
		const broadcast = Path.from(await r.string());
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
	 * Per-frame timestamp scale (units per second). `0` means frames carry no
	 * per-frame timestamps on the wire.
	 */
	timescale: number;
	/**
	 * Boolean hint that this track's payloads are worth compressing. It names no
	 * algorithm: that's negotiated per hop (SETUP) and named per frame. When set,
	 * every FRAME on the track carries a per-frame `Compression` field. Wire values
	 * `>1` are reserved and decode as `true`, so the hint stays additive.
	 */
	compress: boolean;

	constructor({
		priority = 0,
		ordered = true,
		timescale = 0,
		compress = false,
	}: {
		priority?: number;
		ordered?: boolean;
		timescale?: number;
		compress?: boolean;
	}) {
		this.priority = priority;
		this.ordered = ordered;
		this.timescale = timescale;
		this.compress = compress;
	}

	async #encode(w: Writer) {
		await w.u8(this.priority);
		await w.bool(this.ordered);
		await w.u53(this.timescale);
		await w.u53(this.compress ? 1 : 0);
	}

	static async #decode(r: Reader): Promise<TrackInfo> {
		const priority = await r.u8();
		const ordered = await r.bool();
		const timescale = await r.u53();
		// Any non-zero value (including reserved `>1`) is the "worth compressing" hint.
		const compress = (await r.u53()) !== 0;
		return new TrackInfo({ priority, ordered, timescale, compress });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTrack(version);
		return Message.encode(w, (w) => this.#encode(w));
	}

	static async decode(r: Reader, version: Version): Promise<TrackInfo> {
		guardTrack(version);
		return Message.decode(r, (r) => TrackInfo.#decode(r));
	}
}
