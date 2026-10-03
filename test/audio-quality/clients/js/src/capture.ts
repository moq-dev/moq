/** Capture arrivals before the player's container orders or skips them. @module */
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { type Group, Time } from "@moq/net";
import { Effect, type Getter } from "@moq/signals";
import type { Broadcast } from "@moq/watch";
import type { Arrival } from "./schema.ts";

/** The public player inputs needed to observe its existing subscription. */
export type Input = {
	broadcast: Getter<Broadcast | undefined>;
	track: Getter<string | undefined>;
	config: Getter<Catalog.AudioConfig | undefined>;
	maxAge: Getter<Time.Milli>;
};

/** Record frames on the player's connection with its subscription settings. */
export class Capture {
	readonly #signals = new Effect();
	readonly #arrivals: Arrival[] = [];
	readonly #seen = new Set<string>();
	#failure?: string;

	constructor(input: Input) {
		this.#signals.run((effect) => {
			const broadcast = effect.get(input.broadcast);
			const name = effect.get(input.track);
			const config = input.config.peek();
			if (!broadcast || !name || !config) return;
			try {
				const decoder = format(config);
				const active = broadcast.relativeBroadcast(effect, config.broadcast);
				if (!active) return;
				const track = active
					.track(name)
					.subscribe({ priority: Catalog.PRIORITY.audio, maxAge: input.maxAge.peek() });
				const groups = new Set<Group.Consumer>();
				effect.cleanup(() => {
					track.close();
					for (const group of groups) group.close();
				});
				effect.run((inner) =>
					track.update({ priority: Catalog.PRIORITY.audio, maxAge: inner.get(input.maxAge) }),
				);
				effect.spawn(async () => {
					try {
						for (;;) {
							const group = await effect.race(track.recvGroup());
							if (!group) throw new Error("the captured track ended");
							groups.add(group);
							effect.spawn(async () => {
								try {
									for await (const arrival of arrivals(group, decoder)) {
										const key = `${group.sequence}:${arrival[1]}`;
										if (this.#seen.has(key)) continue;
										this.#seen.add(key);
										this.#arrivals.push(arrival);
									}
								} catch (error) {
									if (!effect.abort.aborted) this.#failure ??= String(error);
								} finally {
									groups.delete(group);
								}
							});
						}
					} catch (error) {
						if (!effect.abort.aborted) this.#failure ??= String(error);
					}
				});
			} catch (error) {
				this.#failure ??= String(error);
			}
		});
	}

	/** Return arrivals since the last drain. */
	drain(): Arrival[] {
		return this.#arrivals.splice(0);
	}

	/** Return the failure that stopped capture, if any. */
	error(): string | undefined {
		return this.#failure;
	}

	/** Release the observer without closing the player's subscription. */
	close(): void {
		this.#signals.close();
	}
}

/** Decode the audio container selected by the catalog. */
export function format(config: Catalog.AudioConfig): Container.Format {
	const container = config.container;
	if (container.kind === "legacy") return new Container.Legacy.Format(config);
	if (container.kind === "loc") return new Container.Loc.Format("audio");
	if (container.kind === "cmaf") {
		const init = Uint8Array.from(atob(container.init), (c) => c.charCodeAt(0));
		return new Container.Cmaf.Format(Container.Cmaf.decodeInitSegment(init));
	}
	throw new Error(`unsupported container: ${JSON.stringify(container)}`);
}

/** Stamp frames as their group's stream delivers them, before in-order playback. */
export async function* arrivals(group: Group.Consumer, decoder: Container.Format): AsyncGenerator<Arrival> {
	try {
		for (;;) {
			const next = await group.readFrame();
			const at = performance.now();
			if (!next) return;
			for (const frame of decoder.decode(next.payload)) {
				if (decoder.end?.(frame) !== undefined) continue;
				yield [at, Time.Milli.fromMicro(frame.timestamp), group.sequence];
			}
		}
	} finally {
		group.close();
	}
}
