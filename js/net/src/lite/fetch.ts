import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Duration from "./duration.ts";
import { decodeNode, decodePin, encodeNode, encodePin } from "./instance.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import { hasByteStart, hasFetchMaxDelay, hasFrameBounds, Version } from "./version.ts";

function guardFetch(version: Version) {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			throw new Error("fetch not supported for this version");
		default:
			break;
	}
}

export class Fetch {
	broadcast: Path.Valid;
	/** The publisher instance the subscriber expects. Lite-07+. */
	instance?: Instance.Valid;
	/** The origin node that serves the request, 0 for whichever the publisher selects. Lite-07+. */
	node: bigint;
	track: string;
	priority: number;
	/** Reader freshness budget in whole milliseconds. Lite-07+ only; omission leaves the publisher's max age as the only bound. */
	maxDelay?: number;
	group: bigint;

	/** Index of the first frame to return; 0 is the start of the group. Lite-06+. */
	startFrame: number;

	/**
	 * Offset of the first byte to return within frame {@link startFrame}; 0 is the whole
	 * frame. The response still states that frame's full size. Lite-07+.
	 */
	startByte: number;

	/**
	 * Index of the last frame to return (inclusive), or undefined for through the end
	 * of the group. Lite-06+.
	 */
	endFrame?: number;

	constructor({
		broadcast,
		instance,
		node = 0n,
		track,
		priority,
		maxDelay,
		group,
		startFrame = 0,
		startByte = 0,
		endFrame,
	}: {
		broadcast: Path.Valid;
		instance?: Instance.Valid;
		node?: bigint;
		track: string;
		priority: number;
		maxDelay?: number;
		group: bigint;
		startFrame?: number;
		startByte?: number;
		endFrame?: number;
	}) {
		this.broadcast = broadcast;
		this.instance = instance;
		this.node = node;
		this.track = track;
		this.priority = priority;
		this.maxDelay = maxDelay;
		this.group = group;
		this.startFrame = startFrame;
		this.startByte = startByte;
		this.endFrame = endFrame;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.broadcast));
		await encodePin(w, version, this.instance);
		await encodeNode(w, version, this.node);
		await w.string(this.track);
		await w.u8(toWire(this.priority));
		if (hasFetchMaxDelay(version)) await Duration.encodeOptional(w, this.maxDelay);
		else if (this.maxDelay !== undefined) throw new Error("fetch maxDelay not supported for this version");
		await w.u62(this.group);

		if (!hasFrameBounds(version) && (this.startFrame !== 0 || this.endFrame !== undefined)) {
			// The peer would serve the whole group, including frames we excluded.
			throw new Error("frame bounds not supported for this version");
		}
		if (!hasByteStart(version) && this.startByte !== 0) {
			// The peer would resend the frame from its start, misaligning the reader.
			throw new Error("fetch start byte not supported for this version");
		}
		if (hasFrameBounds(version)) await w.u53(this.startFrame);
		if (hasByteStart(version)) await w.u53(this.startByte);
		if (hasFrameBounds(version)) await w.u53(this.endFrame !== undefined ? this.endFrame + 1 : 0);
	}

	static async #decode(r: Reader, version: Version): Promise<Fetch> {
		const broadcast = Path.decode(await r.string());
		const instance = await decodePin(r, version);
		const node = await decodeNode(r, version);
		const track = await r.string();
		const priority = fromWire(await r.u8());
		const maxDelay = hasFetchMaxDelay(version) ? await Duration.decodeOptional(r) : undefined;
		const group = await r.u62();

		if (!hasFrameBounds(version)) {
			return new Fetch({ broadcast, instance, node, track, priority, maxDelay, group });
		}

		const startFrame = await r.u53();
		const startByte = hasByteStart(version) ? await r.u53() : 0;
		const endFrame = await r.u53();
		if (endFrame > 0 && endFrame - 1 < startFrame) {
			throw new Error("fetch frame range ends before it starts");
		}

		return new Fetch({
			broadcast,
			instance,
			node,
			track,
			priority,
			maxDelay,
			group,
			startFrame,
			startByte,
			endFrame: endFrame > 0 ? endFrame - 1 : undefined,
		});
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardFetch(version);
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Fetch> {
		guardFetch(version);
		return Message.decode(r, (r) => Fetch.#decode(r, version));
	}
}
