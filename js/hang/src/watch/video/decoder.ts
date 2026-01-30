import type * as Moq from "@moq/lite";
import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import * as Container from "../../container";
import * as Hex from "../../util/hex";
import type { BufferedRanges } from "../backend";
import type { Backend, Stats } from "./backend";
import type { Source } from "./source";

// The amount of time to wait before considering the video to be buffering.
const BUFFERING = 500 as Time.Milli;

export type DecoderProps = {
	enabled?: boolean | Signal<boolean>;
};

// The types in VideoDecoderConfig that cause a hard reload.
// ex. codedWidth/Height are optional and can be changed in-band, so we don't want to trigger a reload.
// This way we can keep the current subscription active.
type RequiredDecoderConfig = Omit<Catalog.VideoConfig, "codedWidth" | "codedHeight">;

export class Decoder implements Backend {
	enabled: Signal<boolean>; // Don't download any longer
	source: Source;

	// The current track running, held so we can cancel it when the new track is ready.
	#pending?: Effect;
	#active?: Effect;

	// Expose the current frame to render as a signal
	#frame = new Signal<VideoFrame | undefined>(undefined);
	readonly frame: Getter<VideoFrame | undefined> = this.#frame;

	// The timestamp of the current frame.
	#timestamp = new Signal<Time.Milli | undefined>(undefined);
	readonly timestamp: Getter<Time.Milli | undefined> = this.#timestamp;

	// The display size of the video in pixels, ideally sourced from the catalog.
	#display = new Signal<{ width: number; height: number } | undefined>(undefined);
	readonly display: Getter<{ width: number; height: number } | undefined> = this.#display;

	#buffering = new Signal<boolean>(false);
	readonly buffering: Getter<boolean> = this.#buffering;

	#stats = new Signal<Stats | undefined>(undefined);
	readonly stats: Getter<Stats | undefined> = this.#stats;

	// Empty stub for WebCodecs (no traditional buffering)
	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	constructor(source: Source, props?: DecoderProps) {
		this.enabled = Signal.from(props?.enabled ?? false);

		this.source = source;
		this.source.supported.set(supported); // super hacky

		this.#signals.effect(this.#runPending.bind(this));
		this.#signals.effect(this.#runDisplay.bind(this));
		this.#signals.effect(this.#runBuffering.bind(this));
	}

	#runPending(effect: Effect): void {
		const broadcast = effect.get(this.source.broadcast);
		const enabled = effect.get(this.enabled);
		const track = effect.get(this.source.track);
		const config = effect.get(this.source.config);
		const active = broadcast ? effect.get(broadcast.active) : undefined;

		if (!active || !config || !track || !enabled) {
			// Stop the active track.
			this.#active?.close();
			this.#active = undefined;

			this.#frame.update((prev) => {
				prev?.close();
				return undefined;
			});

			return;
		}

		// Start a new pending effect.
		this.#pending = new Effect();

		// NOTE: If the track catches up in time, it'll remove itself from #pending.
		// We use #pending here on purpose so we only close it when it hasn't caught up yet.
		effect.cleanup(() => this.#pending?.close());

		// Remove the codedWidth/Height from the config to avoid a hard reload if nothing else has changed.
		const { codedWidth: _, codedHeight: __, ...minConfig } = config;

		this.#runTrack(this.#pending, active, track, minConfig);
	}

	#runTrack(effect: Effect, broadcast: Moq.Broadcast, name: string, config: RequiredDecoderConfig): void {
		const sub = broadcast.subscribe(name, Catalog.PRIORITY.video);
		effect.cleanup(() => sub.close());

		effect.cleanup(() => {
			if (this.#active === effect) {
				this.#timestamp.set(undefined);
			}
		});

		const decoder = new VideoDecoder({
			output: async (frame: VideoFrame) => {
				try {
					const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
					if (timestamp < (this.#timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						return;
					}

					if (this.#frame.peek() === undefined) {
						// Render something while we wait for the sync to catch up.
						this.#frame.set(frame.clone());
					}

					const wait = this.source.sync.wait(timestamp).then(() => true);
					const ok = await Promise.race([wait, effect.cancel]);
					if (!ok) return;

					if (timestamp < (this.#timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						// NOTE: This can happen when the ref is updated, such as on playback start.
						return;
					}

					this.#timestamp.set(timestamp);

					this.#frame.update((prev) => {
						prev?.close();
						return frame.clone(); // avoid closing the frame here
					});

					// If the track switch was pending, complete it now.
					if (this.#pending === effect) {
						this.#active?.close();
						this.#active = effect;
						this.#pending = undefined;
					}
				} finally {
					frame.close();
				}
			},
			// TODO bubble up error
			error: (error) => {
				console.error(error);
				effect.close();
			},
		});
		effect.cleanup(() => decoder.close());

		// Input processing - depends on container type
		if (config.container.kind === "cmaf") {
			this.#runCmafTrack(effect, sub, config, decoder);
		} else {
			this.#runLegacyTrack(effect, sub, config, decoder);
		}
	}

	#runLegacyTrack(effect: Effect, sub: Moq.Track, config: RequiredDecoderConfig, decoder: VideoDecoder): void {
		// Create consumer that reorders groups/frames up to the provided latency.
		const consumer = new Container.Legacy.Consumer(sub, {
			latency: this.source.sync.latency,
		});
		effect.cleanup(() => consumer.close());

		decoder.configure({
			...config,
			description: config.description ? Hex.toBytes(config.description) : undefined,
			optimizeForLatency: config.optimizeForLatency ?? true,
			// @ts-expect-error Only supported by Chrome, so the renderer has to flip manually.
			flip: false,
		});

		effect.spawn(async () => {
			for (;;) {
				const next = await Promise.race([consumer.decode(), effect.cancel]);
				if (!next) break;

				// Mark that we received this frame right now.
				this.source.sync.received(Time.Milli.fromMicro(next.timestamp as Time.Micro));

				const chunk = new EncodedVideoChunk({
					type: next.keyframe ? "key" : "delta",
					data: next.data,
					timestamp: next.timestamp,
				});

				// Track both frame count and bytes received for stats in the UI
				this.#stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					timestamp: next.timestamp,
					bytesReceived: (current?.bytesReceived ?? 0) + next.data.byteLength,
				}));

				decoder.decode(chunk);
			}
		});
	}

	#runCmafTrack(effect: Effect, sub: Moq.Track, config: RequiredDecoderConfig, decoder: VideoDecoder): void {
		if (config.container.kind !== "cmaf") return;

		const { timescale } = config.container;
		const description = config.description ? Hex.toBytes(config.description) : undefined;

		// Configure decoder with description from catalog
		decoder.configure({
			codec: config.codec,
			description,
			optimizeForLatency: config.optimizeForLatency ?? true,
			// @ts-expect-error Only supported by Chrome, so the renderer has to flip manually.
			flip: false,
		});

		effect.spawn(async () => {
			// Process data segments
			// TODO: Use a consumer wrapper for CMAF to support latency control
			for (;;) {
				const group = await Promise.race([sub.nextGroup(), effect.cancel]);
				if (!group) break;

				effect.spawn(async () => {
					try {
						for (;;) {
							const segment = await Promise.race([group.readFrame(), effect.cancel]);
							if (!segment) break;

							const samples = Container.Cmaf.decodeDataSegment(segment, timescale);

							for (const sample of samples) {
								const chunk = new EncodedVideoChunk({
									type: sample.keyframe ? "key" : "delta",
									data: sample.data,
									timestamp: sample.timestamp,
								});

								// Mark that we received this frame right now.
								this.source.sync.received(Time.Milli.fromMicro(sample.timestamp as Time.Micro));

								// Track stats
								this.#stats.update((current) => ({
									frameCount: (current?.frameCount ?? 0) + 1,
									timestamp: sample.timestamp,
									bytesReceived: (current?.bytesReceived ?? 0) + sample.data.byteLength,
								}));

								decoder.decode(chunk);
							}
						}
					} finally {
						group.close();
					}
				});
			}
		});
	}

	#runDisplay(effect: Effect): void {
		const catalog = effect.get(this.source.catalog);
		if (!catalog) return;

		const display = catalog.display;
		if (display) {
			effect.set(this.#display, {
				width: display.width,
				height: display.height,
			});
			return;
		}

		const frame = effect.get(this.frame);
		if (!frame) return;

		effect.set(this.#display, {
			width: frame.displayWidth,
			height: frame.displayHeight,
		});
	}

	#runBuffering(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const frame = effect.get(this.frame);
		if (!frame) {
			this.#buffering.set(true);
			return;
		}

		this.#buffering.set(false);

		effect.timer(() => {
			this.#buffering.set(true);
		}, BUFFERING);
	}

	close() {
		this.#frame.update((prev) => {
			prev?.close();
			return undefined;
		});

		this.#signals.close();
	}
}

async function supported(config: Catalog.VideoConfig): Promise<boolean> {
	const description = config.description ? Hex.toBytes(config.description) : undefined;
	const { supported } = await VideoDecoder.isConfigSupported({
		codec: config.codec,
		description,
		optimizeForLatency: config.optimizeForLatency ?? true,
	});

	return supported ?? false;
}
