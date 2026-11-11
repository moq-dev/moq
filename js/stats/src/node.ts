import { type Announce, type Origin, Path } from "@moq/net";
import { PREFIX, parseNodePath } from "./track.ts";

/** A stats node appearing or going away. */
export interface NodeEvent {
	/** The node's name, `""` for a nodeless relay. */
	node: Path.Valid;
	/** The node's stats broadcast path, to follow with `origin.follow(path)`. */
	path: Path.Valid;
	/** Whether the node is announced; `false` once it went away. */
	active: boolean;
}

/** Discovers the stats nodes announced under a prefix. */
export class Nodes {
	#prefix: Path.Valid;
	#announced: Announce.Consumer;

	/** Watch `origin` for nodes under `prefix`, `.stats` by default. */
	constructor(origin: Origin.Reader, prefix: Path.Valid = Path.from(PREFIX)) {
		this.#prefix = prefix;
		this.#announced = origin.announced(Path.Pattern.subtree(Path.join(prefix, Path.from("node"))), {
			hidden: true,
		});
	}

	/** The next node change, or `undefined` once discovery ends. */
	async next(): Promise<NodeEvent | undefined> {
		for (;;) {
			const event = await this.#announced.next();
			if (!event) return undefined;
			const node = parseNodePath(this.#prefix, event.prefix);
			if (node === undefined) continue;
			const active = event.kind !== "absent" && event.kind !== "offline";
			return { node, path: event.prefix, active };
		}
	}

	/** Stop discovering. Idempotent. */
	close(): void {
		this.#announced.close();
	}
}
