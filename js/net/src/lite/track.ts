import type * as Epoch from "../epoch.ts";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { decodeEpoch, encodeEpoch } from "./epoch.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import { hasCompression, hasGroupOrder, hasRefresh, hasTrackStream, Version, wireTimescale } from "./version.ts";

// TRACK_INFO `Compression` codec ids: none, or per-group DEFLATE (draft-lcurley-moq-flate).
const COMPRESSION_NONE = 0;
const COMPRESSION_DEFLATE = 1;

async function decodeCompression(r: Reader): Promise<boolean> {
	const codec = await r.u53();
	switch (codec) {
		case COMPRESSION_NONE:
			return false;
		case COMPRESSION_DEFLATE:
			return true;
		default:
			throw new ProtocolViolation(`unknown compression: ${codec}`);
	}
}

function guardTrack(version: Version) {
	if (!hasTrackStream(version)) throw new Error("track stream not supported for this version");
}

/**
 * TRACK request: the first (and only) subscriber message on a Track Stream (0x6).
 * Asks for a track's immutable publisher properties without subscribing or fetching.
 */
export class Track {
	broadcast: Path.Valid;
	track: string;
	/** The publisher instance the subscriber expects. Lite-07+. */
	epoch?: Epoch.Valid;

	constructor(broadcast: Path.Valid, track: string, epoch?: Epoch.Valid) {
		this.broadcast = broadcast;
		this.track = track;
		this.epoch = epoch;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.broadcast));
		await encodeEpoch(w, version, this.epoch);
		await w.string(this.track);
	}

	static async #decode(r: Reader, version: Version): Promise<Track> {
		const broadcast = Path.decode(await r.string());
		const epoch = await decodeEpoch(r, version);
		const track = await r.string();
		return new Track(broadcast, track, epoch);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTrack(version);
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Track> {
		guardTrack(version);
		return Message.decode(r, (r) => Track.#decode(r, version));
	}
}

/**
 * TRACK_INFO reply: the publisher's sole message on a Track Stream, carrying the
 * track's immutable properties. Fetched once and reused across every SUBSCRIBE and
 * FETCH for the track.
 */
export class TrackInfo {
	/** Refresh byte ratio on lite-07. */
	refresh?: number;
	/** The publisher's tie-break priority for this track. */
	priority: number;
	/**
	 * Publisher Max Age: an upper bound (milliseconds) on how long the publisher
	 * caches a non-latest group past the arrival of a newer one.
	 */
	maxAge?: number;
	/**
	 * Per-frame timestamp scale (units per second), or undefined for an untimed track.
	 * Lite-07 encodes undefined as zero; lite-05/06 have no untimed encoding, so it goes
	 * out as milliseconds (see {@link wireTimescale}).
	 */
	timescale?: Timescale;
	/**
	 * Whether each group's frames are DEFLATE slices of one stream (draft-lcurley-moq-flate),
	 * each FRAME carrying its inflated size. Lite-07 only.
	 */
	compress: boolean;

	constructor({
		priority = 0,
		maxAge,
		timescale,
		refresh,
		compress = false,
	}: {
		priority?: number;
		maxAge?: number;
		timescale?: number;
		refresh?: number;
		compress?: boolean;
	}) {
		if (!Number.isInteger(priority) || priority < -128 || priority > 127) {
			throw new RangeError(`priority must be an integer in -128..=127: ${priority}`);
		}
		if (maxAge !== undefined && (!Number.isSafeInteger(maxAge) || maxAge < 0)) {
			throw new RangeError(`maxAge must be a safe non-negative integer: ${maxAge}`);
		}
		if (refresh !== undefined && (!Number.isInteger(refresh) || refresh < 0 || refresh > 0xffff_ffff))
			throw new RangeError("refresh exceeds u32");
		this.refresh = refresh;
		this.priority = priority;
		this.maxAge = maxAge;
		this.timescale = timescale === undefined ? undefined : Timescale(timescale);
		this.compress = compress;
	}

	async #encode(w: Writer, version: Version) {
		await w.u8(toWire(this.priority));
		// The retired `Ordered` byte: lite-05 keeps it in its layout, written as 0.
		if (hasGroupOrder(version)) await w.bool(false);
		const legacy = version === Version.DRAFT_05 || version === Version.DRAFT_06;
		const age = this.maxAge === undefined ? undefined : BigInt(this.maxAge);
		await w.u62(legacy ? (age ?? BigInt(Number.MAX_SAFE_INTEGER)) : age === undefined ? 0n : age + 1n);
		await w.u53(wireTimescale(version, this.timescale) ?? 0);
		if (hasRefresh(version)) await w.u53(this.refresh === undefined ? 0 : this.refresh + 1);
		if (hasCompression(version)) await w.u53(this.compress ? COMPRESSION_DEFLATE : COMPRESSION_NONE);
	}

	static async #decode(r: Reader, version: Version): Promise<TrackInfo> {
		const priority = fromWire(await r.u8());
		if (hasGroupOrder(version)) await r.bool();
		const encoded = await r.u62();
		const legacy = version === Version.DRAFT_05 || version === Version.DRAFT_06;
		const age = legacy
			? encoded >= BigInt(Number.MAX_SAFE_INTEGER)
				? undefined
				: encoded
			: encoded === 0n
				? undefined
				: encoded - 1n;
		if (age !== undefined && age > BigInt(Number.MAX_SAFE_INTEGER))
			throw new RangeError("maxAge exceeds safe milliseconds");
		const maxAge = age === undefined ? undefined : Number(age);
		// Lite-05/06 have no untimed encoding, so zero is malformed there.
		const scale = await r.u53();
		const timescale = scale === 0 && !legacy ? undefined : Timescale(scale);
		const refresh = hasRefresh(version) ? (await r.u53()) - 1 : undefined;
		const compress = hasCompression(version) ? await decodeCompression(r) : false;
		return new TrackInfo({
			priority,
			maxAge,
			timescale,
			refresh: refresh === -1 ? undefined : refresh,
			compress,
		});
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTrack(version);
		// Re-check after construction: fields are public, and a mutated value must not
		// reach the length-prefixed writer.
		if (!Number.isInteger(this.priority) || this.priority < -128 || this.priority > 127) {
			throw new RangeError(`priority must be an integer in -128..=127: ${this.priority}`);
		}
		if (this.maxAge !== undefined && (!Number.isSafeInteger(this.maxAge) || this.maxAge < 0)) {
			throw new RangeError(`maxAge must be a safe non-negative integer: ${this.maxAge}`);
		}
		if (
			this.refresh !== undefined &&
			(!Number.isInteger(this.refresh) || this.refresh < 0 || this.refresh > 0xffff_ffff)
		)
			throw new RangeError("refresh exceeds u32");
		if (this.timescale !== undefined) Timescale(this.timescale);
		if (this.compress && !hasCompression(version)) {
			throw new Error("a compressed track needs lite-07");
		}
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<TrackInfo> {
		guardTrack(version);
		return Message.decode(r, (r) => TrackInfo.#decode(r, version));
	}
}
