import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { hasFrameBounds, Version } from "./version.ts";

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
	track: string;
	priority: number;
	group: number;

	/** Index of the first frame to return; 0 is the start of the group. Lite-06+. */
	startFrame: number;

	/**
	 * Index of the last frame to return (inclusive), or undefined for through the end
	 * of the group. Lite-06+.
	 */
	endFrame?: number;

	constructor({
		broadcast,
		track,
		priority,
		group,
		startFrame = 0,
		endFrame,
	}: {
		broadcast: Path.Valid;
		track: string;
		priority: number;
		group: number;
		startFrame?: number;
		endFrame?: number;
	}) {
		this.broadcast = broadcast;
		this.track = track;
		this.priority = priority;
		this.group = group;
		this.startFrame = startFrame;
		this.endFrame = endFrame;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(Path.encode(this.broadcast));
		await w.string(this.track);
		await w.u8(this.priority);
		await w.u53(this.group);

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
		const track = await r.string();
		const priority = await r.u8();
		const group = await r.u53();

		if (!hasFrameBounds(version)) {
			return new Fetch({ broadcast, track, priority, group });
		}

		const startFrame = await r.u53();
		const endFrame = await r.u53();
		if (endFrame > 0 && endFrame - 1 < startFrame) {
			throw new Error("fetch frame range ends before it starts");
		}

		return new Fetch({
			broadcast,
			track,
			priority,
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
