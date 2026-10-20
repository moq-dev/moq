import type * as Epoch from "../epoch.ts";
import { ProtocolViolation, reason, SessionError, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Cluster from "./cluster.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { MalformedAuthToken, tokenIntoRequest } from "./token.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Which advertisement extensions a session negotiated: what its PUBLISH_NAMESPACE and
 * NAMESPACE messages carry.
 */
export interface Negotiated {
	/** MoQ Cluster: every advertisement carries HOP_PATH, and may carry ROUTE_COST. */
	cluster: boolean;
	/** MoQ Epoch: an advertisement may carry EPOCH. */
	epoch: boolean;
}

/** A session that negotiated neither advertisement extension. */
export const NEGOTIATED_NONE: Negotiated = { cluster: false, epoch: false };

/** Whether NAMESPACE takes the extended form, with a Parameters field. */
export function extended(negotiated: Negotiated): boolean {
	return negotiated.cluster || negotiated.epoch;
}

/** The extension parameters one advertisement carries. */
export interface AdvertParams {
	/** The MoQ Cluster parameters (see {@link Cluster}), set exactly on a session that negotiated it. */
	cluster?: Cluster.Advert;
	/** The MoQ Epoch parameter: the publisher instance the route serves, if it names one. */
	epoch?: Epoch.Valid;
}

/** Write an advertisement's extension parameters into a fresh block. */
export function advertIntoParams(advert: AdvertParams): Parameters {
	const params = advert.cluster ? Cluster.intoParams(advert.cluster) : new Parameters();
	params.epoch = advert.epoch;
	return params;
}

/**
 * Read an advertisement's Parameters field, checked against the list of the message that
 * carries it (PUBLISH_NAMESPACE also allows a token), expecting what the session negotiated.
 *
 * Every way this fails is one to close the session over: a block that does not parse, a
 * negotiated cluster session that omits HOP_PATH, or an EPOCH on a session that did not
 * negotiate MoQ Epoch. A request token's own refusal keeps its type.
 */
export async function decodeAdvert(
	r: Reader,
	version: IetfVersion,
	message: "namespace" | "publish-namespace",
	negotiated: Negotiated,
): Promise<AdvertParams> {
	let params: Parameters;
	try {
		params = await Parameters.decode(r, version, message);
	} catch (err) {
		// Keep a violation's own code, such as a request token's, and a refusal of the request alone.
		if (err instanceof ProtocolViolation || err instanceof MalformedAuthToken) throw err;
		throw new ProtocolViolation(reason(err), { cause: err });
	}

	const epoch = params.epoch;
	// An unknown parameter to a session that never offered the extension.
	if (epoch !== undefined && !negotiated.epoch) throw new ProtocolViolation("EPOCH without MoQ Epoch");
	return { cluster: negotiated.cluster ? Cluster.fromParams(params) : undefined, epoch };
}

// In draft-14, ANNOUNCE is renamed to PUBLISH_NAMESPACE
export class PublishNamespace {
	static id = 0x06;

	requestId: bigint;
	trackNamespace: Path.Valid;

	/** The extension parameters, which a session that negotiated none leaves empty. */
	params: AdvertParams;

	/** The AUTHORIZATION TOKEN to send, a value agreed out of band. Decoding leaves it unset. */
	token?: Uint8Array;

	constructor({
		requestId,
		trackNamespace,
		params = {},
		token,
	}: { requestId: bigint; trackNamespace: Path.Valid; params?: AdvertParams; token?: Uint8Array }) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.params = params;
		this.token = token;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		if (version === Version.DRAFT_17) {
			await w.u62(0n); // required_request_id_delta: only 0 supported until stream-per-request
		}
		await Namespace.encode(w, this.trackNamespace);
		const params = advertIntoParams(this.params);
		if (this.token) tokenIntoRequest(params, this.token, version);
		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	/**
	 * Decode the message, expecting the parameters of the extensions the session negotiated.
	 *
	 * The negotiation is session state rather than anything in the message, so the caller
	 * supplies it. See {@link decodeAdvert} for what it refuses.
	 */
	static async decode(
		r: Reader,
		version: IetfVersion,
		negotiated: Negotiated = NEGOTIATED_NONE,
	): Promise<PublishNamespace> {
		return Message.decode(r, (rd) => PublishNamespace.#decode(rd, version, negotiated));
	}

	static async #decode(r: Reader, version: IetfVersion, negotiated: Negotiated): Promise<PublishNamespace> {
		const requestId = await r.u62();
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta
		}
		const trackNamespace = await Namespace.decode(r);
		const params = await decodeAdvert(r, version, "publish-namespace", negotiated);
		return new PublishNamespace({ requestId, trackNamespace, params });
	}
}

/**
 * REQUEST_UPDATE (0x02) on a PUBLISH_NAMESPACE stream: the cluster parameters that
 * changed (draft-lcurley-moq-cluster, Updating an Advertisement).
 *
 * Draft-17+ only: the extension negotiates on nothing earlier. The publisher sends one when
 * a forwarded route's price or hop chain moves behind the same original publisher.
 */
export class PublishNamespaceUpdate {
	static id = 0x02;

	/** The update's own Request ID; every REQUEST_UPDATE consumes one. */
	requestId: bigint;
	/** Draft-16 only: the PUBLISH_NAMESPACE this updates, which later drafts name by stream. */
	namespaceRequestId?: bigint;
	update: Cluster.Update;
	/** A new AUTHORIZATION TOKEN, renewing the request's. Decoding leaves it unset. */
	token?: Uint8Array;

	constructor({
		requestId,
		namespaceRequestId,
		update,
		token,
	}: {
		requestId: bigint;
		namespaceRequestId?: bigint;
		update: Cluster.Update;
		token?: Uint8Array;
	}) {
		this.requestId = requestId;
		this.namespaceRequestId = namespaceRequestId;
		this.update = update;
		this.token = token;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		PublishNamespaceUpdate.#supported(version);
		await w.u62(this.requestId);
		if (version === Version.DRAFT_16) {
			if (this.namespaceRequestId === undefined) {
				throw new Error("a draft-16 REQUEST_UPDATE names its PUBLISH_NAMESPACE");
			}
			await w.u62(this.namespaceRequestId);
		}
		if (version === Version.DRAFT_17) {
			await w.u62(0n); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
		}
		const params = Cluster.updateIntoParams(this.update);
		if (this.token) tokenIntoRequest(params, this.token, version);
		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	/**
	 * Decode the message. A truncated or trailing body, a draft with no such message, or a
	 * malformed parameter block surfaces as a {@link ProtocolViolation}, which the session
	 * dispatch closes over. A stream reset or session ending remains transport-local.
	 */
	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceUpdate> {
		try {
			return await Message.decode(r, (rd) => PublishNamespaceUpdate.#decode(rd, version));
		} catch (err) {
			if (
				err instanceof ProtocolViolation ||
				err instanceof SessionError ||
				err instanceof StreamError ||
				err instanceof MalformedAuthToken
			)
				throw err;
			if (typeof err === "object" && err !== null) {
				const source = (err as { source?: unknown }).source;
				if (source === "session" || source === "stream") throw err;
			}
			throw new ProtocolViolation(reason(err), { cause: err });
		}
	}

	static #supported(version: IetfVersion) {
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15) {
			throw new Error("REQUEST_UPDATE on PUBLISH_NAMESPACE requires draft-16+");
		}
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceUpdate> {
		PublishNamespaceUpdate.#supported(version);
		const requestId = await r.u62();
		const namespaceRequestId = version === Version.DRAFT_16 ? await r.u62() : undefined;
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
		}
		const params = await Parameters.decode(r, version, "publish-namespace");
		// An epoch is fixed for the life of an advertisement, so no update carries one.
		if (params.epoch !== undefined) throw new ProtocolViolation("EPOCH on a PUBLISH_NAMESPACE update");
		return new PublishNamespaceUpdate({ requestId, namespaceRequestId, update: Cluster.updateFromParams(params) });
	}
}

export class PublishNamespaceOk {
	static id = 0x07;

	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishNamespaceOk> {
		return Message.decode(r, PublishNamespaceOk.#decode);
	}

	static async #decode(r: Reader): Promise<PublishNamespaceOk> {
		const requestId = await r.u62();
		return new PublishNamespaceOk({ requestId });
	}
}

export class PublishNamespaceError {
	static id = 0x08;

	requestId: bigint;
	errorCode: number;
	reasonPhrase: string;

	constructor({
		requestId,
		errorCode,
		reasonPhrase,
	}: { requestId: bigint; errorCode: number; reasonPhrase: string }) {
		this.requestId = requestId;
		this.errorCode = errorCode;
		this.reasonPhrase = reasonPhrase;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
		await w.u62(BigInt(this.errorCode));
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishNamespaceError> {
		return Message.decode(r, PublishNamespaceError.#decode);
	}

	static async #decode(r: Reader): Promise<PublishNamespaceError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();
		return new PublishNamespaceError({ requestId, errorCode, reasonPhrase });
	}
}

// Removed in d17
export class PublishNamespaceCancel {
	static id = 0x0c;

	trackNamespace: Path.Valid;
	requestId: bigint; // v16: uses request_id instead of track_namespace
	errorCode: number;
	reasonPhrase: string;

	constructor({
		trackNamespace = "" as Path.Valid,
		errorCode = 0,
		reasonPhrase = "",
		requestId = 0n,
	}: {
		trackNamespace?: Path.Valid;
		errorCode?: number;
		reasonPhrase?: string;
		requestId?: bigint;
	} = {}) {
		this.trackNamespace = trackNamespace;
		this.requestId = requestId;
		this.errorCode = errorCode;
		this.reasonPhrase = reasonPhrase;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			throw new Error("PublishNamespaceCancel removed in draft-17+");
		}
		if (version === Version.DRAFT_16) {
			await w.u62(this.requestId);
		} else {
			await Namespace.encode(w, this.trackNamespace);
		}
		await w.u62(BigInt(this.errorCode));
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceCancel> {
		return Message.decode(r, (rd) => PublishNamespaceCancel.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceCancel> {
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			throw new Error("PublishNamespaceCancel removed in draft-17+");
		}
		let trackNamespace = "" as Path.Valid;
		let requestId = 0n;
		if (version === Version.DRAFT_16) {
			requestId = await r.u62();
		} else {
			trackNamespace = await Namespace.decode(r);
		}
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();
		return new PublishNamespaceCancel({ trackNamespace, errorCode, reasonPhrase, requestId });
	}
}

// In draft-14, UNANNOUNCE is renamed to PUBLISH_NAMESPACE_DONE
// In draft-16, uses request_id instead of track_namespace
// Removed in d17
export class PublishNamespaceDone {
	static readonly id = 0x09;

	trackNamespace: Path.Valid;
	requestId: bigint; // v16: uses request_id instead of track_namespace

	constructor({
		trackNamespace = "" as Path.Valid,
		requestId = 0n,
	}: {
		trackNamespace?: Path.Valid;
		requestId?: bigint;
	} = {}) {
		this.trackNamespace = trackNamespace;
		this.requestId = requestId;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			throw new Error("PublishNamespaceDone removed in draft-17+");
		}
		if (version === Version.DRAFT_16) {
			await w.u62(this.requestId);
		} else {
			await Namespace.encode(w, this.trackNamespace);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceDone> {
		return Message.decode(r, (rd) => PublishNamespaceDone.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceDone> {
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			throw new Error("PublishNamespaceDone removed in draft-17+");
		}
		if (version === Version.DRAFT_16) {
			const requestId = await r.u62();
			return new PublishNamespaceDone({ requestId });
		}
		const trackNamespace = await Namespace.decode(r);
		return new PublishNamespaceDone({ trackNamespace });
	}
}
