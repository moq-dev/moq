import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import { type IetfVersion, Version } from "./version.ts";

export class Publish {
	static id = 0x1d;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	trackAlias: bigint;
	groupOrder: number;
	priority?: number;
	contentExists: boolean;
	largest: { groupId: bigint; objectId: bigint } | undefined;
	forward: boolean;
	properties?: Properties.Properties;
	parameters?: Parameters;

	constructor({
		requestId,
		trackNamespace,
		trackName,
		trackAlias,
		groupOrder,
		priority,
		contentExists,
		largest,
		forward,
		properties,
		parameters,
	}: {
		requestId: bigint;
		trackNamespace: Path.Valid;
		trackName: string;
		trackAlias: bigint;
		groupOrder: number;
		priority?: number;
		contentExists: boolean;
		largest: { groupId: bigint; objectId: bigint } | undefined;
		forward: boolean;
		properties?: Properties.Properties;
		parameters?: Parameters;
	}) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.trackAlias = trackAlias;
		this.groupOrder = groupOrder;
		this.priority = priority;
		this.contentExists = contentExists;
		this.largest = largest;
		this.forward = forward;
		this.properties = properties;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);
		await w.u62(this.trackAlias);

		if (this.contentExists !== !!this.largest) {
			throw new Error("contentExists and largest must both be true or false");
		}
		const params = this.parameters ?? new Parameters();
		// The publisher's group order is a DEFAULT_PUBLISHER_GROUP_ORDER track property: a
		// draft-18 or 19 peer closes the session on a GROUP_ORDER parameter here.
		params.forward = this.forward;
		if (this.largest) {
			params.largest = this.largest;
		}
		await params.encode(w, version);

		// Track Properties are the final field, so nothing may follow.
		await Properties.encode(
			w,
			this.properties ?? { groupOrder: this.groupOrder, priority: this.priority },
			version,
		);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Publish> {
		return Message.decode(r, (mr) => Publish.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Publish> {
		const requestId = await r.u62();
		const { namespace: trackNamespace, name: trackName } = await Namespace.decodeTrack(r);
		const trackAlias = await r.u62();

		const params = await Parameters.decode(r, version, "publish");
		const properties = await Properties.decode(r, version);
		// GROUP_ORDER is legal here from draft-22; draft-18 closes the session.
		const groupOrder = properties.groupOrder ?? params.groupOrder ?? 0x02;
		const forward = params.forward ?? true;
		const largest = params.largest;
		return new Publish({
			requestId,
			trackNamespace,
			trackName,
			trackAlias,
			groupOrder,
			priority: properties.priority,
			properties,
			parameters: params,
			contentExists: !!largest,
			largest,
			forward,
		});
	}
}

/** PUBLISH_DONE status codes this implementation distinguishes. Stable across drafts 18 through 22. */
export const PublishDoneStatus = {
	INTERNAL_ERROR: 0x0,
	UNAUTHORIZED: 0x1,
	TRACK_ENDED: 0x2,
	/** Unassigned in draft-22. */
	SUBSCRIPTION_ENDED: 0x3,
	UPDATE_FAILED: 0x8,
} as const;

/**
 * Whether a PUBLISH_DONE status ends the subscription cleanly, so its data streams in flight still
 * drain, rather than aborting it. Only TRACK_ENDED also ends the track.
 */
export function publishDoneClean(statusCode: number, version: IetfVersion): boolean {
	if (statusCode === PublishDoneStatus.TRACK_ENDED) return true;
	if (statusCode !== PublishDoneStatus.SUBSCRIPTION_ENDED) return false;
	switch (version) {
		case Version.DRAFT_18:
			return true;
		default:
			return false;
	}
}

export class PublishDone {
	static readonly id = 0x0b;

	statusCode: number;
	/**
	 * How many data streams the publisher opened for the subscription, fill streams included.
	 * A hint: a peer may send 0 or the "unknown" sentinel regardless.
	 */
	streamCount: bigint;
	reasonPhrase: string;

	constructor({
		statusCode,
		streamCount = 0n,
		reasonPhrase,
	}: { statusCode: number; streamCount?: bigint; reasonPhrase: string }) {
		this.statusCode = statusCode;
		this.streamCount = streamCount;
		this.reasonPhrase = reasonPhrase;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(BigInt(this.statusCode));
		await w.u62(this.streamCount);
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishDone> {
		return Message.decode(r, PublishDone.#decode);
	}

	static async #decode(r: Reader): Promise<PublishDone> {
		const statusCode = Number(await r.u62());
		const streamCount = await r.u62();
		const reasonPhrase = await r.string();

		return new PublishDone({ statusCode, streamCount, reasonPhrase });
	}
}
