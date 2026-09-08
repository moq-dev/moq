import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Filter from "./filter.ts";
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
			// The whole filter, not just its tag: an absolute filter carries a Start Location
			// and an End Group after it, and skipping those desyncs the parameters that follow.
			await Filter.decodeInline(r);
			await Parameters.decode(r, version); // parameters
		} else {
			// v15+: just parameters
			await Parameters.decode(r, version);
		}

		return new TrackStatusRequest({ requestId, trackNamespace, trackName });
	}
}

/**
 * TRACK_STATUS_OK (0x0e), the draft-14 answer to a successful TRACK_STATUS.
 *
 * The body is byte-identical to SUBSCRIBE_OK, with Track Alias 0, so `SubscribeOk` encodes
 * it and only the type differs. Draft-15 and later answer with REQUEST_OK instead, which is
 * why 0x0e is free to mean NAMESPACE_DONE from draft-16 on.
 */
export const TRACK_STATUS_OK_ID = 0x0e;

/**
 * TRACK_STATUS_ERROR (0x0f), the draft-14 refusal of a TRACK_STATUS.
 *
 * The body is byte-identical to SUBSCRIBE_ERROR. Draft-15 and later refuse with
 * REQUEST_ERROR instead.
 */
export const TRACK_STATUS_ERROR_ID = 0x0f;
