/** Capture arrivals before the player's container orders or skips them. @module */

import { Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Cmaf from "@moq/hang/cmaf";
import * as Container from "@moq/hang/container";
import { type Group, Error as NetError } from "@moq/net";
import { Effect, type Getter } from "@moq/signals";
import type { Broadcast, Delay } from "@moq/watch";
import { frameDuration, packetDuration } from "../../../../../js/watch/src/audio/config.ts";
import type { Arrival } from "./schema.ts";

/**
 * The least max delay the decoder subscribes with at "auto": the estimator's 2 s ceiling, so a frame
 * later than the target is still observed (doc/concept/audio-jitter.md). The observer asks for the
 * same, or it would drop exactly the late frames the estimator measures.
 */
const AUTO_MAX_DELAY = Milli(2000);

/** The public player inputs needed to observe its existing subscription. */
export type Input = {
	broadcast: Getter<Broadcast | undefined>;
	track: Getter<string | undefined>;
	config: Getter<Catalog.AudioConfig | undefined>;
	/** The configured delay, which decides whether the decoder subscribes past `maxDelay`. */
	delay: Getter<Delay>;
	maxDelay: Getter<Milli>;
};

/** The max delay the decoder subscribes with, which the observer matches. */
function subscribed(delay: Delay, maxDelay: Milli): Milli {
	return delay === "auto" ? Milli(Math.max(maxDelay, AUTO_MAX_DELAY)) : maxDelay;
}

/** Record frames on the player's connection with its subscription settings. */
export class Capture {
	readonly #signals = new Effect();
	readonly #arrivals: Arrival[] = [];
	readonly #seen = new Set<string>();
	readonly #readers = new Set<Promise<void>>();
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
				const track = active.track(name).subscribe({
					priority: Catalog.PRIORITY.audio,
					maxDelay: subscribed(input.delay.peek(), input.maxDelay.peek()),
				});
				const groups = new Set<Group.Consumer>();
				effect.cleanup(() => {
					track.close();
					for (const group of groups) group.close();
				});
				effect.run((inner) =>
					track.update({
						priority: Catalog.PRIORITY.audio,
						maxDelay: subscribed(inner.get(input.delay), inner.get(input.maxDelay)),
					}),
				);
				effect.spawn(async () => {
					try {
						for (;;) {
							const group = await effect.race(track.recvGroup());
							if (!group) throw new Error("the captured track ended");
							groups.add(group);
							// Closing a group ends its reader cleanly after the buffered frames, so any
							// other error here is a real failure, even one surfacing during close.
							const reader = (async () => {
								try {
									for await (const arrival of arrivals(group, { decoder, config })) {
										// A resubscribe after a rerun re-delivers cached frames with a later stamp.
										const key = `${group.sequence}:${arrival[1]}`;
										if (this.#seen.has(key)) continue;
										this.#seen.add(key);
										this.#arrivals.push(arrival);
									}
								} catch (error) {
									// A reset or lagging group truncates its tail, as it does for the player.
									if (error instanceof NetError.Stream) return;
									this.#failure ??= String(error);
								} finally {
									groups.delete(group);
								}
							})();
							this.#readers.add(reader);
							void reader.then(() => this.#readers.delete(reader));
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

	/** Release the observer without closing the player's subscription, once buffered frames are recorded. */
	async close(): Promise<void> {
		this.#signals.close();
		await Promise.all(this.#readers);
	}
}

/** Decode the audio container selected by the catalog. */
export function format(config: Catalog.AudioConfig): Container.Format {
	const container = config.container;
	if (container.kind === "legacy") return new Container.Legacy.Format(config);
	if (container.kind === "loc") return new Container.Loc.Format("audio");
	if (container.kind === "cmaf") {
		const init = Uint8Array.from(atob(container.init), (c) => c.charCodeAt(0));
		return new Cmaf.Format(Cmaf.decodeInitSegment(init));
	}
	throw new Error(`unsupported container: ${JSON.stringify(container)}`);
}

/** Stamp frames as their group's stream delivers them, before in-order playback. */
export async function* arrivals(
	group: Group.Consumer,
	{ decoder, config }: { decoder: Container.Format; config: Catalog.AudioConfig },
): AsyncGenerator<Arrival> {
	try {
		for (;;) {
			const next = await group.readFrame();
			const at = performance.now();
			if (!next) return;
			for (const frame of decoder.decode(next.payload, next.timestamp)) {
				if (decoder.end?.(frame) !== undefined) continue;
				const duration = packetDuration(config.codec, frame) ?? frameDuration(config);
				if (duration === undefined || !Number.isFinite(duration) || duration <= 0) {
					throw new Error(`no encoded frame duration for ${config.codec}`);
				}
				yield [at, Milli.fromMicro(frame.timestamp), group.sequence, duration];
			}
		}
	} finally {
		group.close();
	}
}
