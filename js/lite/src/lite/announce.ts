import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class Announce {
	suffix: Path.Valid;
	active: boolean;
	hops: bigint[];

	constructor(props: { suffix: Path.Valid; active: boolean; hops?: bigint[] }) {
		this.suffix = props.suffix;
		this.active = props.active;
		this.hops = props.hops ?? [];
	}

	async #encode(w: Writer, version: Version) {
		await w.bool(this.active);
		await w.string(this.suffix);

		switch (version) {
			case Version.DRAFT_03:
				await w.u53(this.hops.length);
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				// DRAFT_04+: encode array of OriginId
				await w.u53(this.hops.length);
				for (const hop of this.hops) {
					await w.u62(hop);
				}
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Announce> {
		const active = await r.bool();
		const suffix = Path.from(await r.string());

		const hops: bigint[] = [];
		switch (version) {
			case Version.DRAFT_03: {
				// Read count but don't know actual IDs
				await r.u53();
				break;
			}
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default: {
				// DRAFT_04+: decode array of OriginId
				const count = await r.u53();
				for (let i = 0; i < count; i++) {
					hops.push(await r.u62());
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
	withoutOrigin?: bigint;

	constructor(prefix: Path.Valid, withoutOrigin?: bigint) {
		this.prefix = prefix;
		this.withoutOrigin = withoutOrigin;
	}

	async #encode(w: Writer, version: Version) {
		await w.string(this.prefix);

		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default:
				// DRAFT_04+: encode withoutOrigin as varint (0 = no filter)
				await w.u62(this.withoutOrigin ?? 0n);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<AnnounceInterest> {
		const prefix = Path.from(await r.string());

		let withoutOrigin: bigint | undefined;
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
			case Version.DRAFT_03:
				break;
			default: {
				// DRAFT_04+: decode withoutOrigin
				const val = await r.u62();
				withoutOrigin = val !== 0n ? val : undefined;
				break;
			}
		}

		return new AnnounceInterest(prefix, withoutOrigin);
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
			case Version.DRAFT_03:
			case Version.DRAFT_04:
				throw new Error("announce init not supported for this version");
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
