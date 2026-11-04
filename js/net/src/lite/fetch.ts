import type { Milli } from "@moq/clock";
import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { decodePin, encodePin } from "./instance.ts";
import * as Message from "./message.ts";
import { fromWire, toWire } from "./priority.ts";
import { hasFetchMaxDelay, hasFrameBounds, Version } from "./version.ts";

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
	track: string;
	priority: number;
	/** Reader freshness budget. Lite-07+ only; omission leaves the publisher's max age as the only bound. */
	maxDelay?: Milli;
	group: bigint;

	/** Index of the first frame to return; 0 is the start of the group. Lite-06+. */
	startFrame: number;

	/**
	 * Index of the last frame to return (inclusive), or undefined for through the end
	 * of the group. Lite-06+.
	 */
	endFrame?: number;

	constructor({
		broadcast,
		instance,
		track,
		priority,
		maxDelay,
		group,
		startFrame = 0,
		endFrame,
	}: {
		broadcast: Path.Valid;
		instance?: Instance.Valid;
		track: string;
		priority: number;
		maxDelay?: Milli;
		group: bigint;
		startFrame?: number;
		endFrame?: number;
	}) {
		this.broadcast = broadcast;
		this.instance = instance;
		this.track = track;
		this.priority = priority;
		this.maxDelay = maxDelay;
		this.group = group;
		this.startFrame = startFrame;
		this.endFrame = endFrame;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.broadcast));
		await encodePin(w, version, this.instance);
		await w.string(this.track);
		await w.u8(toWire(this.priority));
		if (hasFetchMaxDelay(version)) {
			if (this.maxDelay !== undefined && (!Number.isSafeInteger(this.maxDelay) || this.maxDelay < 0))
				throw new RangeError("max delay exceeds safe milliseconds");
			await w.u62(this.maxDelay === undefined ? 0n : BigInt(this.maxDelay) + 1n);
		} else if (this.maxDelay !== undefined) throw new Error("fetch maxDelay not supported for this version");
		await w.u62(this.group);

		if (hasFrameBounds(version)) {
			await w.u53(this.startFrame);
			await w.u53(this.endFrame !== undefined ? this.endFrame + 1 : 0);
		} else if (this.startFrame !== 0 || this.endFrame !== undefined) {
			// The peer would serve the whole group, including frames we excluded.
			throw new Error("frame bounds not supported for this version");
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Fetch> {
		const broadcast = Path.decode(await r.string());
		const instance = await decodePin(r, version);
		const track = await r.string();
		const priority = fromWire(await r.u8());
		// The optional encoding adds one, so even a safe millisecond budget can exceed u53.
		const encodedMaxDelay = hasFetchMaxDelay(version) ? await r.u62() : 0n;
		if (encodedMaxDelay > BigInt(Number.MAX_SAFE_INTEGER) + 1n)
			throw new RangeError("max delay exceeds safe milliseconds");
		const maxDelay = encodedMaxDelay === 0n ? undefined : (Number(encodedMaxDelay - 1n) as Milli);
		const group = await r.u62();

		if (!hasFrameBounds(version)) {
			return new Fetch({ broadcast, instance, track, priority, maxDelay, group });
		}

		const startFrame = await r.u53();
		const endFrame = await r.u53();
		if (endFrame > 0 && endFrame - 1 < startFrame) {
			throw new Error("fetch frame range ends before it starts");
		}

		return new Fetch({
			broadcast,
			instance,
			track,
			priority,
			maxDelay,
			group,
			startFrame,
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
