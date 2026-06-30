import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { hasTrackStream, type Version } from "./version.ts";

/**
 * Sent by a subscriber on a Track stream (moq-lite-05+) to request a track's
 * immutable publisher properties without subscribing or fetching.
 */
export class Track {
	broadcast: Path.Valid;
	track: string;

	constructor(props: { broadcast: Path.Valid; track: string }) {
		this.broadcast = props.broadcast;
		this.track = props.track;
	}

	async #encode(w: Writer) {
		await w.string(this.broadcast);
		await w.string(this.track);
	}

	static async #decode(r: Reader): Promise<Track> {
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		return new Track({ broadcast, track });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		if (!hasTrackStream(version)) throw new Error("TRACK requires moq-lite-05+");
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<Track> {
		if (!hasTrackStream(version)) throw new Error("TRACK requires moq-lite-05+");
		return Message.decode(r, Track.#decode);
	}
}

/**
 * The publisher's reply on a Track stream (moq-lite-05+): the track's immutable
 * properties. Sent once, then the publisher FINs the stream.
 */
export class TrackInfo {
	priority: number;
	ordered: boolean;
	maxLatency: number;
	timescale: number;

	constructor(props: { priority?: number; ordered?: boolean; maxLatency?: number; timescale: number }) {
		this.priority = props.priority ?? 0;
		this.ordered = props.ordered ?? false;
		this.maxLatency = props.maxLatency ?? 0;
		this.timescale = props.timescale;
	}

	async #encode(w: Writer) {
		await w.u8(this.priority);
		await w.bool(this.ordered);
		await w.u53(this.maxLatency);
		await w.u53(this.timescale);
	}

	static async #decode(r: Reader): Promise<TrackInfo> {
		const priority = await r.u8();
		const ordered = await r.bool();
		const maxLatency = await r.u53();
		const timescale = await r.u53();
		// A zero timescale is a protocol violation: every track has a media timeline.
		if (timescale === 0) throw new Error("TRACK_INFO timescale must be non-zero");
		return new TrackInfo({ priority, ordered, maxLatency, timescale });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		if (!hasTrackStream(version)) throw new Error("TRACK_INFO requires moq-lite-05+");
		// Reject a zero timescale on encode too (mirrors the Rust side), so an invalid
		// TrackInfo fails fast on the sender rather than only at the peer's decoder.
		if (this.timescale === 0) throw new Error("TRACK_INFO timescale must be non-zero");
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<TrackInfo> {
		if (!hasTrackStream(version)) throw new Error("TRACK_INFO requires moq-lite-05+");
		return Message.decode(r, TrackInfo.#decode);
	}
}
