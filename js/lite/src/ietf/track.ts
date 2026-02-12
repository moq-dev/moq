import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import type { IetfVersion } from "./version.ts";

export class TrackStatusRequest {
	static id = 0x0d;

	trackNamespace: Path.Valid;
	trackName: string;

	constructor({ trackNamespace, trackName }: { trackNamespace: Path.Valid; trackName: string }) {
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
	}

	async #encode(w: Writer): Promise<void> {
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<TrackStatusRequest> {
		return Message.decode(r, TrackStatusRequest.#decode);
	}

	static async #decode(r: Reader): Promise<TrackStatusRequest> {
		const trackNamespace = await Namespace.decode(r);
		const trackName = await r.string();
		return new TrackStatusRequest({ trackNamespace, trackName });
	}
}

// Track status message for communicating track-level state
export class TrackStatus {
	static id = 0x0e;

	trackNamespace: Path.Valid;
	trackName: string;
	statusCode: number;
	lastGroupId: bigint;
	lastObjectId: bigint;

	constructor({
		trackNamespace,
		trackName,
		statusCode,
		lastGroupId,
		lastObjectId,
	}: {
		trackNamespace: Path.Valid;
		trackName: string;
		statusCode: number;
		lastGroupId: bigint;
		lastObjectId: bigint;
	}) {
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.statusCode = statusCode;
		this.lastGroupId = lastGroupId;
		this.lastObjectId = lastObjectId;
	}

	async #encode(w: Writer): Promise<void> {
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);
		await w.u62(BigInt(this.statusCode));
		await w.u62(this.lastGroupId);
		await w.u62(this.lastObjectId);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<TrackStatus> {
		return Message.decode(r, TrackStatus.#decode);
	}

	static async #decode(r: Reader): Promise<TrackStatus> {
		const trackNamespace = await Namespace.decode(r);
		const trackName = await r.string();
		const statusCode = Number(await r.u62());
		const lastGroupId = await r.u62();
		const lastObjectId = await r.u62();

		return new TrackStatus({ trackNamespace, trackName, statusCode, lastGroupId, lastObjectId });
	}

	// Track status codes
	static readonly STATUS_IN_PROGRESS = 0x00;
	static readonly STATUS_NOT_FOUND = 0x01;
	static readonly STATUS_NOT_AUTHORIZED = 0x02;
	static readonly STATUS_ENDED = 0x03;
}
