import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

// we only support Group Order descending
const GROUP_ORDER = 0x02;

export class TrackStatusRequest {
	static id = 0x0d;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;

	constructor({
		requestId,
		trackNamespace,
		trackName,
	}: { requestId: bigint; trackNamespace: Path.Valid; trackName: string }) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		if (version === Version.DRAFT_17) {
			await w.u62(0n); // required_request_id_delta — always 0, not supported
		}
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);

		if (version === Version.DRAFT_14) {
			await w.u8(0); // subscriber_priority
			await w.u8(GROUP_ORDER); // group_order
			await w.bool(false); // forward
			await w.u53(0x2); // filter_type = LargestObject
			await w.u53(0); // no parameters
		} else {
			// v15+: just parameters
			const params = new Parameters();
			await params.encode(w, version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<TrackStatusRequest> {
		return Message.decode(r, (mr) => TrackStatusRequest.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<TrackStatusRequest> {
		const requestId = await r.u62();
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta
		}
		const trackNamespace = await Namespace.decode(r);
		const trackName = await r.string();

		if (version === Version.DRAFT_14) {
			await r.u8(); // subscriber_priority
			await r.u8(); // group_order
			await r.bool(); // forward
			await r.u53(); // filter_type
			await Parameters.decode(r, version); // parameters
		} else {
			// v15+: just parameters
			await Parameters.decode(r, version);
		}

		return new TrackStatusRequest({ requestId, trackNamespace, trackName });
	}
}

// Track status response (0x0E) — v14 only (TRACK_STATUS_OK)
// In v15+, the response to TrackStatusRequest is RequestOk (0x07)
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
