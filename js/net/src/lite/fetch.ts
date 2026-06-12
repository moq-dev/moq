import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

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
	/**
	 * The 0-based index of the first frame to return; the publisher skips all
	 * earlier frames. `0` returns the entire group. Draft-05+ only; older drafts
	 * always return the whole group.
	 */
	frameStart: number;

	constructor(broadcast: Path.Valid, track: string, priority: number, group: number, frameStart = 0) {
		this.broadcast = broadcast;
		this.track = track;
		this.priority = priority;
		this.group = group;
		this.frameStart = frameStart;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(this.broadcast);
		await w.string(this.track);
		await w.u8(this.priority);
		await w.u53(this.group);

		switch (version) {
			case Version.DRAFT_03:
			case Version.DRAFT_04:
				break;
			default:
				await w.u53(this.frameStart);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Fetch> {
		const broadcast = Path.from(await r.string());
		const track = await r.string();
		const priority = await r.u8();
		const group = await r.u53();

		let frameStart = 0;
		switch (version) {
			case Version.DRAFT_03:
			case Version.DRAFT_04:
				break;
			default:
				frameStart = await r.u53();
				break;
		}

		return new Fetch(broadcast, track, priority, group, frameStart);
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
