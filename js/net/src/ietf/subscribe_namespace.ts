import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import type { IetfVersion } from "./version.ts";

// SUBSCRIBE_NAMESPACE message (type 0x50).
export class SubscribeNamespace {
	static id = 0x50;

	namespace: Path.Valid;
	requestId: bigint;

	constructor({ namespace, requestId }: { namespace: Path.Valid; requestId: bigint }) {
		this.namespace = namespace;
		this.requestId = requestId;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.namespace);
		await new Parameters().encode(w, version);
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
		await Parameters.decode(r, version, "subscribe-namespace");

		return new SubscribeNamespace({ namespace, requestId });
	}
}

/// SUBSCRIBE_TRACKS message ID (0x51) introduced in draft-18 (#1542).
///
/// moq-lite does not implement PUBLISH replication through a CDN, which is the
/// only thing SUBSCRIBE_TRACKS enables. We never send it and refuse each one
/// with REQUEST_ERROR NOT_SUPPORTED.
export const SUBSCRIBE_TRACKS_ID = 0x51;

/** NAMESPACE message (0x08), sent on the SUBSCRIBE_NAMESPACE bidi stream. */
export class SubscribeNamespaceEntry {
	static id = 0x08;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeNamespaceEntry> {
		return Message.decode(r, SubscribeNamespaceEntry.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeNamespaceEntry> {
		const suffix = await Namespace.decode(r);
		return new SubscribeNamespaceEntry({ suffix });
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
