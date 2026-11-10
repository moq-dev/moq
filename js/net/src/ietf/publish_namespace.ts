import { ProtocolViolation, reason, SessionError, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { MalformedAuthToken, tokenIntoRequest } from "./token.ts";
import type { IetfVersion } from "./version.ts";

/**
 * Read a PUBLISH_NAMESPACE's Parameters field. A malformed block closes the session; a request
 * token's own refusal keeps its type.
 */
async function decodeParams(r: Reader, version: IetfVersion): Promise<void> {
	try {
		await Parameters.decode(r, version, "publish-namespace");
	} catch (err) {
		// Keep a violation's own code, such as a request token's, and a refusal of the request alone.
		if (err instanceof ProtocolViolation || err instanceof MalformedAuthToken) throw err;
		throw new ProtocolViolation(reason(err), { cause: err });
	}
}

export class PublishNamespace {
	static id = 0x06;

	requestId: bigint;
	trackNamespace: Path.Valid;

	/** The AUTHORIZATION TOKEN to send, a value agreed out of band. Decoding leaves it unset. */
	token?: Uint8Array;

	constructor({
		requestId,
		trackNamespace,
		token,
	}: { requestId: bigint; trackNamespace: Path.Valid; token?: Uint8Array }) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.token = token;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.trackNamespace);
		const params = new Parameters();
		if (this.token) tokenIntoRequest(params, this.token);
		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	/** Decode the message. See {@link decodeParams} for what it refuses. */
	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespace> {
		return Message.decode(r, (rd) => PublishNamespace.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespace> {
		const requestId = await r.u62();
		const trackNamespace = await Namespace.decode(r);
		await decodeParams(r, version);
		return new PublishNamespace({ requestId, trackNamespace });
	}
}

/**
 * REQUEST_UPDATE (0x02) on a PUBLISH_NAMESPACE stream, renewing the request's
 * AUTHORIZATION TOKEN.
 */
export class PublishNamespaceUpdate {
	static id = 0x02;

	/** The update's own Request ID; every REQUEST_UPDATE consumes one. */
	requestId: bigint;
	/** A new AUTHORIZATION TOKEN, renewing the request's. Decoding leaves it unset. */
	token?: Uint8Array;

	constructor({ requestId, token }: { requestId: bigint; token?: Uint8Array }) {
		this.requestId = requestId;
		this.token = token;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		const params = new Parameters();
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
		await Parameters.decode(r, version, "publish-namespace");
		return new PublishNamespaceUpdate({ requestId });
	}
}
