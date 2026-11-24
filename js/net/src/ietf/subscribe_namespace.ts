import { ProtocolViolation } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import {
	type AdvertParams,
	advertIntoParams,
	decodeAdvert,
	extended,
	NEGOTIATED_NONE,
	type Negotiated,
} from "./publish_namespace.ts";
import { type IetfVersion, isDraft23 } from "./version.ts";

// SUBSCRIBE_NAMESPACE message (type 0x50).
export class SubscribeNamespace {
	static id = 0x50;

	namespace: Path.Valid;
	requestId: bigint;
	/** MoQ Hidden: also advertise hidden namespaces. Only sent to a peer that declared it. */
	hidden: boolean;

	constructor({
		namespace,
		requestId,
		hidden = false,
	}: { namespace: Path.Valid; requestId: bigint; hidden?: boolean }) {
		this.namespace = namespace;
		this.requestId = requestId;
		this.hidden = hidden;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.namespace);
		const params = new Parameters();
		params.hidden = this.hidden;
		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeNamespace> {
		return Message.decode(r, (rd) => SubscribeNamespace.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeNamespace> {
		const requestId = await r.u62();
		const namespace = await Namespace.decode(r);
		const params = await Parameters.decode(r, version, "subscribe-namespace");

		return new SubscribeNamespace({ namespace, requestId, hidden: params.hidden });
	}
}

/// SUBSCRIBE_TRACKS message ID (0x51) introduced in draft-18 (#1542).
///
/// moq-lite does not implement PUBLISH replication through a CDN, which is the
/// only thing SUBSCRIBE_TRACKS enables. We never send it and refuse each one
/// with REQUEST_ERROR NOT_SUPPORTED.
export const SUBSCRIBE_TRACKS_ID = 0x51;

/**
 * NAMESPACE message (0x08), sent on the SUBSCRIBE_NAMESPACE bidi stream.
 *
 * The base message carries only the suffix, and from draft-23 an empty Parameters field. A
 * session that negotiated the MoQ Cluster extension uses the extended form instead, whose
 * Parameters carry the advertisement's HOP_PATH and ROUTE_COST (see `cluster.ts`).
 */
export class SubscribeNamespaceEntry {
	static id = 0x08;

	suffix: Path.Valid;

	/**
	 * The extension parameters. Set selects the extended form, `undefined` the base one.
	 * An endpoint must not append them on a session that negotiated neither extension.
	 */
	params?: AdvertParams;

	constructor({ suffix, params }: { suffix: Path.Valid; params?: AdvertParams }) {
		this.suffix = suffix;
		this.params = params;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await Namespace.encode(w, this.suffix);
		if (this.params) await advertIntoParams(this.params).encode(w, version);
		// Draft-23 (section 9.15) always has the block; it defines no parameter of its own.
		else if (isDraft23(version)) await w.u53(0);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	/**
	 * Decode the message, expecting the extended form when the session negotiated an
	 * extension that defines it. See {@link PublishNamespace.decode}.
	 */
	static async decode(
		r: Reader,
		version: IetfVersion,
		negotiated: Negotiated = NEGOTIATED_NONE,
	): Promise<SubscribeNamespaceEntry> {
		return Message.decode(r, (rd) => SubscribeNamespaceEntry.#decode(rd, version, negotiated));
	}

	static async #decode(r: Reader, version: IetfVersion, negotiated: Negotiated): Promise<SubscribeNamespaceEntry> {
		const suffix = await Namespace.decode(r);
		if (!extended(negotiated)) {
			// Draft-23 defines no NAMESPACE parameter, and one without a length cannot be skipped.
			if (isDraft23(version) && (await r.u53()) !== 0) {
				throw new ProtocolViolation("NAMESPACE carries a parameter no negotiated extension defines");
			}
			return new SubscribeNamespaceEntry({ suffix });
		}

		return new SubscribeNamespaceEntry({ suffix, params: await decodeAdvert(r, version, "namespace", negotiated) });
	}
}

/// NAMESPACE_DONE message (0x0E), sent on the SUBSCRIBE_NAMESPACE bidi stream.
export class SubscribeNamespaceEntryDone {
	static id = 0x0e;

	suffix: Path.Valid;

	constructor({ suffix }: { suffix: Path.Valid }) {
		this.suffix = suffix;
	}

	async #encode(w: Writer): Promise<void> {
		await Namespace.encode(w, this.suffix);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeNamespaceEntryDone> {
		return Message.decode(r, SubscribeNamespaceEntryDone.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeNamespaceEntryDone> {
		const suffix = await Namespace.decode(r);
		return new SubscribeNamespaceEntryDone({ suffix });
	}
}
