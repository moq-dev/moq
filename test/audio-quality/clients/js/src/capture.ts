/** Capture arrivals before the player's container orders or skips them. @module */

import { Duration, Instant, now } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Cmaf from "@moq/hang/cmaf";
import * as Container from "@moq/hang/container";
import { type Group, Error as NetError, type Track } from "@moq/net";
import { Effect, type Getter } from "@moq/signals";
import type { Broadcast } from "@moq/watch";
import { frameDuration, packetDuration } from "../../../../../js/watch/src/audio/config.ts";
import { maxDelay } from "../../../../../js/watch/src/audio/latency.ts";
import type { Arrival } from "./schema.ts";

/** The public player inputs needed to observe its existing subscription. */
export type Input = {
	broadcast: Getter<Broadcast | undefined>;
	track: Getter<string | undefined>;
	config: Getter<Catalog.AudioConfig | undefined>;
	/**
	 * The terms of the max delay the decoder subscribes with, which the observer matches, codec warmup
	 * included, or it would drop exactly the late frames the estimator measures
	 * (doc/concept/audio-jitter.md).
	 */
	delay: Getter<Duration>;
	maxDelay: Getter<Duration>;
};

/** Record frames on the player's connection with its subscription settings. */
export class Capture {
	readonly #signals = new Effect();
	readonly #arrivals: Arrival[] = [];
	readonly #seen = new Set<string>();
	readonly #readers = new Set<Promise<void>>();
	#failure?: string;
	#largest?: number;

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
				// The decoder's live join: its delay behind the live edge and its max, both with the warmup.
				const asked = { priority: Catalog.PRIORITY.audio };
				const playout = (delay: Duration, max: Duration, warmup: number): Container.Delay => ({
					min: delay.add(Duration.millis(warmup)),
					max: maxDelay({ delay, maxDelay: max }).add(Duration.millis(warmup)),
				});
				const track = active
					.track(name)
					.subscribe(
						Container.subscription(
							playout(input.delay.peek(), input.maxDelay.peek(), config.warmup ?? 0),
							asked,
						),
					);
				const groups = new Set<Group.Consumer>();
				const tuned = Promise.withResolvers<void>();
				effect.spawn(async () => {
					try {
						// The first subscription's: a resubscribe after a rerun joins a track already flowing.
						this.#largest ??= await joined(track, tuned.promise);
					} catch (error) {
						if (!effect.abort.aborted) this.#failure ??= String(error);
					}
				});
				effect.cleanup(() => {
					track.close();
					for (const group of groups) group.close();
				});
				effect.run((inner) => {
					const delay = playout(
						inner.get(input.delay),
						inner.get(input.maxDelay),
						inner.get(input.config)?.warmup ?? 0,
					);
					track.update(Container.update(delay, track.subscription.peek() ?? {}, asked));
				});
				effect.spawn(async () => {
					try {
						for (;;) {
							const group = await effect.race(track.recvGroup());
							if (!group) throw new Error("the captured track ended");
							tuned.resolve();
							groups.add(group);
							// Closing a group ends its reader cleanly after the buffered frames, so any
							// other error here is a real failure, even one surfacing during close.
							const reader = (async () => {
								try {
									for await (const arrival of arrivals(group, { decoder, config })) {
										// A resubscribe after a rerun re-delivers cached frames with a later stamp.
										const key = `${group.start}:${arrival[1]}`;
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

	/** The publisher's largest group as the subscription began, once known. */
	largest(): number | undefined {
		return this.#largest;
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

/**
 * The publisher's largest group as `track`'s subscription began, read the way the container consumer
 * reads it: once the track's info or `tuned`, its first group, shows the subscription established, so
 * live groups landing after it don't set it.
 */
export async function joined(track: Track.Subscriber, tuned: Promise<unknown>): Promise<number | undefined> {
	await Promise.race([track.info().catch(() => undefined), tuned]);
	const largest = await track.largest();
	return largest === undefined ? undefined : Number(largest.group);
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
			const at = now().since(Instant.ZERO).asMillis();
			if (!next) return;
			for (const frame of decoder.decode(next.payload, next.timestamp)) {
				if (decoder.end?.(frame) !== undefined) continue;
				const duration = packetDuration(config.codec, frame) ?? frameDuration(config);
				if (duration === undefined || duration.isZero()) {
					throw new Error(`no encoded frame duration for ${config.codec}`);
				}
				yield [at, frame.timestamp.asMillis(), Number(group.start), duration.asMillis()];
			}
		}
	} finally {
		group.close();
	}
}
