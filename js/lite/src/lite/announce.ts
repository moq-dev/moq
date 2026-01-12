import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";

export class Announce {
	suffix: Path.Valid;
	active: boolean;

	constructor(suffix: Path.Valid, active: boolean) {
		this.suffix = suffix;
		this.active = active;
	}

	async #encode(w: Writer) {
		await w.bool(this.active);
		await w.string(this.suffix);
	}

	static async #decode(r: Reader): Promise<Announce> {
		const active = await r.bool();
		const suffix = Path.from(await r.string());
		return new Announce(suffix, active);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<Announce> {
		return Message.decode(r, Announce.#decode);
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

export class AnnounceInit {
	suffixes: Path.Valid[];

	constructor(paths: Path.Valid[]) {
		this.suffixes = paths;
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

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<AnnounceInit> {
		return Message.decode(r, AnnounceInit.#decode);
	}
}
