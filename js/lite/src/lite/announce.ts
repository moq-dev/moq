import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

const MAX_HOPS = 256;

export class Announce {
	suffix: Path.Valid;
	active: boolean;
	hops: number[];

	constructor(props: { suffix: Path.Valid; active: boolean; hops?: number[] }) {
		this.suffix = props.suffix;
		this.active = props.active;
		this.hops = props.hops ?? [];
	}

	async #encode(w: Writer, version: Version) {
		await w.bool(this.active);
		await w.string(this.suffix);

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03:
				await w.u53(this.hops.length);
				break;
			default:
				// Lite04+: hop count + individual hop IDs
				await w.u53(this.hops.length);
				for (const id of this.hops) {
					await w.u53(id);
				}
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Announce> {
		const active = await r.bool();
		const suffix = Path.from(await r.string());

		let hops: number[] = [];
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03: {
				const count = await r.u53();
				if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
				hops = new Array(count).fill(0);
				break;
			}
			default: {
				// Lite04+: hop count + individual hop IDs
				const count = await r.u53();
				if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
				hops = [];
				for (let i = 0; i < count; i++) {
					hops.push(await r.u53());
				}
				break;
			}
		}

		return new Announce({ suffix, active, hops });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Announce> {
		return Message.decode(r, (r) => Announce.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<Announce | undefined> {
		return Message.decodeMaybe(r, (r) => Announce.#decode(r, version));
	}
}

export class AnnounceInterest {
	prefix: Path.Valid;
	excludeHop: number;

	constructor(prefix: Path.Valid, excludeHop = 0) {
		this.prefix = prefix;
		this.excludeHop = excludeHop;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(this.prefix);
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				// Lite04+: exclude_hop field
				await w.u53(this.excludeHop);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceInterest> {
		const prefix = Path.from(await r.string());
		let excludeHop = 0;
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				excludeHop = await r.u53();
				break;
		}
		return new AnnounceInterest(prefix, excludeHop);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceInterest> {
		return Message.decode(r, (r) => AnnounceInterest.#decode(r, version));
	}
}

/// Sent after setup to communicate the initially announced paths.
///
/// Used by Draft01/Draft02 only. Draft03+ uses individual Announce messages instead.
export class AnnounceInit {
	suffixes: Path.Valid[];

	constructor(paths: Path.Valid[]) {
		this.suffixes = paths;
	}

	static #guard(version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				throw new Error("announce init not supported for this version");
		}
	}

	async #encode(w: Writer) {
		await w.u53(this.suffixes.length);
		for (const path of this.suffixes) {
			await w.string(path);
		}
	}

	static async #decode(r: Reader): Promise<AnnounceInit> {
		const count = await r.u53();
		const suffixes: Path.Valid[] = [];
		for (let i = 0; i < count; i++) {
			suffixes.push(Path.from(await r.string()));
		}
		return new AnnounceInit(suffixes);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		AnnounceInit.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<AnnounceInit> {
		AnnounceInit.#guard(version);
		return Message.decode(r, AnnounceInit.#decode);
	}
}
