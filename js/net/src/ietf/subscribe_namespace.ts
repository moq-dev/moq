import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

// True for the drafts that use the legacy 0x11 SUBSCRIBE_NAMESPACE message.
// Draft-18+ uses the renumbered 0x50 message instead.
function isLegacyVersion(version: IetfVersion): boolean {
	switch (version) {
		case Version.DRAFT_14:
		case Version.DRAFT_15:
		case Version.DRAFT_16:
		case Version.DRAFT_17:
			return true;
		default:
			return false;
	}
}

// SUBSCRIBE_NAMESPACE message (draft-18+, type 0x50).
//
// Draft-18 renumbered the message from 0x11 to 0x50 and dropped the Subscribe
// Options field when it split SUBSCRIBE_TRACKS (0x51) off into its own message
// type (#1542). Draft-14 through draft-17 use SubscribeNamespaceLegacy.
export class SubscribeNamespace {
	static id = 0x50;

	namespace: Path.Valid;
	requestId: bigint;

	constructor({ namespace, requestId }: { namespace: Path.Valid; requestId: bigint }) {
		this.namespace = namespace;
		this.requestId = requestId;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (isLegacyVersion(version)) {
			throw new Error(`SUBSCRIBE_NAMESPACE (0x50) is draft-18+ only, not ${version}`);
		}
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
		if (isLegacyVersion(version)) {
			throw new Error(`SUBSCRIBE_NAMESPACE (0x50) is draft-18+ only, not ${version}`);
		}
		const requestId = await r.u62();
		const namespace = await Namespace.decode(r);
		await Parameters.decode(r, version);

		return new SubscribeNamespace({ namespace, requestId });
	}
}

// SUBSCRIBE_NAMESPACE message for draft-14 through draft-17 (type 0x11).
//
// In v16 this moves from the control stream to its own bidi stream. Draft-16/17
// carry a Subscribe Options field (NAMESPACE vs TRACKS); draft-17 additionally
// prefixes a Required Request ID delta (removed in draft-18 per #1615).
// Draft-18+ uses SubscribeNamespace.
export class SubscribeNamespaceLegacy {
	static id = 0x11;

	namespace: Path.Valid;
	requestId: bigint;
	subscribeOptions: number; // v16/v17: default 0x01 (NAMESPACE only)

	constructor({
		namespace,
		requestId,
		subscribeOptions = 1,
	}: {
		namespace: Path.Valid;
		requestId: bigint;
		subscribeOptions?: number;
	}) {
		this.namespace = namespace;
		this.requestId = requestId;
		this.subscribeOptions = subscribeOptions;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (!isLegacyVersion(version)) {
			throw new Error(`legacy SUBSCRIBE_NAMESPACE (0x11) is draft-14..17 only, not ${version}`);
		}
		await w.u62(this.requestId);
		if (version === Version.DRAFT_17) {
			await w.u62(0n); // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}
		await Namespace.encode(w, this.namespace);
		if (version === Version.DRAFT_16 || version === Version.DRAFT_17) {
			await w.u53(this.subscribeOptions);
		}
		await new Parameters().encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeNamespaceLegacy> {
		return Message.decode(r, (rd) => SubscribeNamespaceLegacy.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeNamespaceLegacy> {
		if (!isLegacyVersion(version)) {
			throw new Error(`legacy SUBSCRIBE_NAMESPACE (0x11) is draft-14..17 only, not ${version}`);
		}
		const requestId = await r.u62();
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
		}
		const namespace = await Namespace.decode(r);
		let subscribeOptions = 1;
		if (version === Version.DRAFT_16 || version === Version.DRAFT_17) {
			subscribeOptions = await r.u53();
		}
		await Parameters.decode(r, version);

		return new SubscribeNamespaceLegacy({ namespace, requestId, subscribeOptions });
	}
}

/// SUBSCRIBE_TRACKS message ID (0x51) introduced in draft-18 (#1542).
///
/// moq-lite does not implement PUBLISH replication through a CDN, which is the
/// only thing SUBSCRIBE_TRACKS enables. We never send it and reject it loudly
/// on receipt rather than silently ignoring, since the peer would otherwise
/// wait forever for a REQUEST_OK.
export const SUBSCRIBE_TRACKS_ID = 0x51;

export class SubscribeNamespaceOk {
	static id = 0x12;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeNamespaceOk> {
		return Message.decode(r, SubscribeNamespaceOk.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeNamespaceOk> {
		const requestId = await r.u62();
		return new SubscribeNamespaceOk({ requestId });
	}
}

export class SubscribeNamespaceError {
	static id = 0x13;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeNamespaceError> {
		return Message.decode(r, SubscribeNamespaceError.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeNamespaceError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();

		return new SubscribeNamespaceError({ requestId, errorCode, reasonPhrase });
	}
}

export class UnsubscribeNamespace {
	static id = 0x14;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<UnsubscribeNamespace> {
		return Message.decode(r, UnsubscribeNamespace.#decode);
	}

	static async #decode(r: Reader): Promise<UnsubscribeNamespace> {
		const requestId = await r.u62();
		return new UnsubscribeNamespace({ requestId });
	}
}

/// NAMESPACE message (0x08) — v16 only, sent on SUBSCRIBE_NAMESPACE bidi stream
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

/// NAMESPACE_DONE message (0x0E) — v16 only, sent on SUBSCRIBE_NAMESPACE bidi stream
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

/// PUBLISH_BLOCKED message (0x0F) — draft-17 only, sent on SUBSCRIBE_NAMESPACE bidi stream
export class PublishBlocked {
	static id = 0x0f;

	suffix: Path.Valid;
	trackName: string;

	constructor({ suffix, trackName }: { suffix: Path.Valid; trackName: string }) {
		this.suffix = suffix;
		this.trackName = trackName;
	}

	async #encode(w: Writer): Promise<void> {
		await Namespace.encode(w, this.suffix);
		await w.string(this.trackName);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishBlocked> {
		return Message.decode(r, PublishBlocked.#decode);
	}

	static async #decode(r: Reader): Promise<PublishBlocked> {
		const suffix = await Namespace.decode(r);
		const trackName = await r.string();
		return new PublishBlocked({ suffix, trackName });
	}
}

// Backward compatibility aliases
export const SubscribeAnnounces = SubscribeNamespace;
export const SubscribeAnnouncesOk = SubscribeNamespaceOk;
export const SubscribeAnnouncesError = SubscribeNamespaceError;
export const UnsubscribeAnnounces = UnsubscribeNamespace;
