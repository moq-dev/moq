import { ProtocolViolation, reason, SessionError, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Cluster from "./cluster.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { MalformedAuthToken, tokenIntoRequest } from "./token.ts";
import type { IetfVersion } from "./version.ts";

/**
 * Which advertisement extensions a session negotiated: what its PUBLISH_NAMESPACE and
 * NAMESPACE messages carry.
 */
export interface Negotiated {
	/** MoQ Cluster: every advertisement carries HOP_PATH, and may carry ROUTE_COST. */
	cluster: boolean;
}

/** A session without the advertisement extension. */
export const NEGOTIATED_NONE: Negotiated = { cluster: false };

/** Whether NAMESPACE takes the extended form, with a Parameters field. */
export function extended(negotiated: Negotiated): boolean {
	return negotiated.cluster;
}

/** The extension parameters one advertisement carries. */
export interface AdvertParams {
	/** The MoQ Cluster parameters (see {@link Cluster}), set exactly on a session that negotiated it. */
	cluster?: Cluster.Advert;
}

/** Write an advertisement's extension parameters into a fresh block. */
export function advertIntoParams(advert: AdvertParams): Parameters {
	const params = new Parameters();
	if (advert.cluster) Cluster.intoParams(advert.cluster, params);
	return params;
}

/**
 * Read an advertisement's Parameters field, checked against the list of the message that
 * carries it (PUBLISH_NAMESPACE also allows a token), expecting what the session negotiated.
 *
 * A malformed block or a negotiated cluster session omitting HOP_PATH closes the
 * session. A request token's own refusal keeps its type.
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

	return { cluster: negotiated.cluster ? Cluster.fromParams(params) : undefined };
}

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
		await Namespace.encode(w, this.trackNamespace);
		const params = advertIntoParams(this.params);
		if (this.token) tokenIntoRequest(params, this.token);
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
		const trackNamespace = await Namespace.decode(r);
		const params = await decodeAdvert(r, version, "publish-namespace", negotiated);
		return new PublishNamespace({ requestId, trackNamespace, params });
	}
}

/**
 * REQUEST_UPDATE (0x02) on a PUBLISH_NAMESPACE stream: the cluster parameters that
 * changed (draft-lcurley-moq-cluster, Updating an Advertisement).
 *
 * The publisher sends one when a forwarded route's price or hop chain moves behind the same
 * original publisher.
 */
export class PublishNamespaceUpdate {
	static id = 0x02;

	/** The update's own Request ID; every REQUEST_UPDATE consumes one. */
	requestId: bigint;
	update: Cluster.Update;
	/** A new AUTHORIZATION TOKEN, renewing the request's. Decoding leaves it unset. */
	token?: Uint8Array;

	constructor({ requestId, update, token }: { requestId: bigint; update: Cluster.Update; token?: Uint8Array }) {
		this.requestId = requestId;
		this.update = update;
		this.token = token;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		const params = new Parameters();
		Cluster.updateIntoParams(this.update, params);
		if (this.token) tokenIntoRequest(params, this.token);
		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	/**
	 * Decode the message. A truncated or trailing body, or a malformed parameter block surfaces as a {@link ProtocolViolation}, which the session
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

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceUpdate> {
		const requestId = await r.u62();
		const params = await Parameters.decode(r, version, "publish-namespace");
		return new PublishNamespaceUpdate({ requestId, update: Cluster.updateFromParams(params) });
	}
}
