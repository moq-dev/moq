import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class Announce {
	suffix: Path.Valid;
	active: boolean;
	hops: number;

	constructor(props: { suffix: Path.Valid; active: boolean; hops?: number }) {
		this.suffix = props.suffix;
		this.active = props.active;
		this.hops = props.hops ?? 0;
	}

	async #encode(w: Writer, version: Version) {
		await w.bool(this.active);
		await w.string(this.suffix);

		switch (version) {
			case Version.DRAFT_03:
				await w.u53(this.hops);
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				unreachable(version);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Announce> {
		const active = await r.bool();
		const suffix = Path.from(await r.string());

		let hops = 0;
		switch (version) {
			case Version.DRAFT_03:
				hops = await r.u53();
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				unreachable(version);
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

	constructor(prefix: Path.Valid) {
		this.prefix = prefix;
	}

	async #encode(w: Writer) {
		await w.string(this.prefix);
	}

	static async #decode(r: Reader): Promise<AnnounceInterest> {
		const prefix = Path.from(await r.string());
		return new AnnounceInterest(prefix);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<AnnounceInterest> {
		return Message.decode(r, AnnounceInterest.#decode);
	}
}

/// Sent after setup to communicate the initially announced paths.
///
/// Used by Draft01/Draft02 only. Draft03 uses individual Announce messages instead.
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
			case Version.DRAFT_03:
				throw new Error("announce init not supported for Draft03");
			default:
				unreachable(version);
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
