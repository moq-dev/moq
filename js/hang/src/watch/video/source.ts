import type * as Moq from "@moq/lite";
import type { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import * as Container from "../../container";
import { PRIORITY } from "../../publish/priority";
import * as Hex from "../../util/hex";
import { Latency } from "../../util/latency";

export type SourceProps = {
	enabled?: boolean | Signal<boolean>;

	// Additional buffer in milliseconds on top of the catalog's minBuffer (default: 100ms).
	// The effective latency = catalog.minBuffer + buffer
	// Increase this if experiencing stuttering due to network jitter.
	buffer?: Time.Milli | Signal<Time.Milli>;
};

export type Target = {
	// The desired size of the video in pixels.
	pixels?: number;

	// Optional manual override for the selected rendition name.
	rendition?: string;

	// TODO bitrate
};

// The types in VideoDecoderConfig that cause a hard reload.
// ex. codedWidth/Height are optional and can be changed in-band, so we don't want to trigger a reload.
// This way we can keep the current subscription active.
type RequiredDecoderConfig = Omit<Catalog.VideoConfig, "codedWidth" | "codedHeight">;

type BufferStatus = { state: "empty" | "filled" };

type SyncStatus = {
	state: "ready" | "wait";
	bufferDuration?: number;
};

export interface VideoStats {
	frameCount: number;
	timestamp: number;
	bytesReceived: number;
}

// Only count it as buffering if we had to sleep for 200ms or more before rendering the next frame.
// Unfortunately, this has to be quite high because of b-frames.
// TODO Maybe we need to detect b-frames and make this dynamic?
const MIN_SYNC_WAIT_MS = 200 as Time.Milli;

// The maximum number of concurrent b-frames that we support.
const MAX_BFRAMES = 10;

// Responsible for switching between video tracks and buffering frames.
export class Source {
	broadcast: Signal<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>; // Don't download any longer

	catalog = new Signal<Catalog.Video | undefined>(undefined);

	// The tracks supported by our video decoder.
	#supported = new Signal<Record<string, Catalog.VideoConfig>>({});

	// The track we chose from the supported tracks.
	#selected = new Signal<string | undefined>(undefined);
	#selectedConfig = new Signal<RequiredDecoderConfig | undefined>(undefined);

	// The config of the active rendition.
	#config = new Signal<Catalog.VideoConfig | undefined>(undefined);
	readonly config: Getter<Catalog.VideoConfig | undefined> = this.#config;

	// The name of the active rendition.
	active = new Signal<string | undefined>(undefined);

	// The current track running, held so we can cancel it when the new track is ready.
	#pending?: Effect;
	#active?: Effect;

	// Used as a tiebreaker when there are multiple tracks (HD vs SD).
	target = new Signal<Target | undefined>(undefined);

	// Expose the current frame to render as a signal
	frame = new Signal<VideoFrame | undefined>(undefined);

	// Additional buffer in milliseconds (on top of catalog's minBuffer).
	buffer: Signal<Time.Milli>;

	// Computed latency = catalog.minBuffer + buffer
	#latency: Latency;
	readonly latency: Getter<Time.Milli>;

	// The display size of the video in pixels, ideally sourced from the catalog.
	display = new Signal<{ width: number; height: number } | undefined>(undefined);

	// Whether to flip the video horizontally.
	flip = new Signal<boolean | undefined>(undefined);

	// Used to convert PTS to wall time.
	#reference: DOMHighResTimeStamp | undefined;

	bufferStatus = new Signal<BufferStatus>({ state: "empty" });
	syncStatus = new Signal<SyncStatus>({ state: "ready" });

	#stats = new Signal<VideoStats | undefined>(undefined);
	readonly stats: Getter<VideoStats | undefined> = this.#stats;

	#signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		catalog: Signal<Catalog.Root | undefined>,
		props?: SourceProps,
	) {
		this.broadcast = broadcast;
		this.buffer = Signal.from(props?.buffer ?? (100 as Time.Milli));
		this.enabled = Signal.from(props?.enabled ?? false);

		// Compute effective latency from catalog.minBuffer + buffer
		this.#latency = new Latency({
			buffer: this.buffer,
			config: this.#config,
		});
		this.latency = this.#latency.combined;

		this.#signals.effect((effect) => {
			const c = effect.get(catalog)?.video;
			effect.set(this.catalog, c);
			effect.set(this.flip, c?.flip);
		});

		this.#signals.effect(this.#runSupported.bind(this));
		this.#signals.effect(this.#runSelected.bind(this));
		this.#signals.effect(this.#runPending.bind(this));
		this.#signals.effect(this.#runDisplay.bind(this));
		this.#signals.effect(this.#runBuffer.bind(this));
	}

	#runSupported(effect: Effect): void {
		const renditions = effect.get(this.catalog)?.renditions ?? {};

		effect.spawn(async () => {
			const supported: Record<string, Catalog.VideoConfig> = {};

			for (const [name, rendition] of Object.entries(renditions)) {
				// For CMAF, we get description from init segment, so skip validation here
				// and just check if the codec is supported
				if (rendition.container.kind === "cmaf") {
					const { supported: valid } = await VideoDecoder.isConfigSupported({
						codec: rendition.codec,
						optimizeForLatency: rendition.optimizeForLatency ?? true,
					});
					if (valid) supported[name] = rendition;
					continue;
				}

				// Legacy container: validate with description from catalog
				const description = rendition.description ? Hex.toBytes(rendition.description) : undefined;

				const { supported: valid } = await VideoDecoder.isConfigSupported({
					...rendition,
					description,
					optimizeForLatency: rendition.optimizeForLatency ?? true,
				});
				if (valid) supported[name] = rendition;
			}

			if (Object.keys(supported).length === 0 && Object.keys(renditions).length > 0) {
				console.warn("no supported renditions found, available: ", renditions);
			}

			this.#supported.set(supported);
		});
	}

	#runSelected(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const supported = effect.get(this.#supported);
		const target = effect.get(this.target);

		const manual = target?.rendition;
		const selected = manual && manual in supported ? manual : this.#selectRendition(supported, target);
		if (!selected) return;

		effect.set(this.#selected, selected);

		// Store the full config for latency computation
		effect.set(this.#config, supported[selected]);

		// Remove the codedWidth/Height from the config to avoid a hard reload if nothing else has changed.
		const config = { ...supported[selected], codedWidth: undefined, codedHeight: undefined };
		effect.set(this.#selectedConfig, config);
	}

	#runPending(effect: Effect): void {
		const broadcast = effect.get(this.broadcast);
		const enabled = effect.get(this.enabled);
		const selected = effect.get(this.#selected);
		const config = effect.get(this.#selectedConfig);

		if (!broadcast || !selected || !config || !enabled) {
			// Stop the active track.
			this.#active?.close();
			this.#active = undefined;

			this.frame.update((prev) => {
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

		this.#runTrack(this.#pending, broadcast, selected, config);
	}

	#runTrack(effect: Effect, broadcast: Moq.Broadcast, name: string, config: RequiredDecoderConfig): void {
		const sub = broadcast.subscribe(name, PRIORITY.video); // TODO use priority from catalog
		effect.cleanup(() => sub.close());

		// We need a queue because VideoDecoder doesn't block on a Promise returned by output.
		// NOTE: We will drain this queue almost immediately, so the highWaterMark is just a safety net.
		const queue = new TransformStream<VideoFrame, VideoFrame>(
			undefined,
			{ highWaterMark: MAX_BFRAMES },
			{ highWaterMark: MAX_BFRAMES },
		);

		const writer = queue.writable.getWriter();
		effect.cleanup(() => writer.close());

		const reader = queue.readable.getReader();
		effect.cleanup(async () => {
			// Drain any remaining frames in the queue to prevent memory leaks
			try {
				let result = await reader.read();
				while (!result.done) {
					result.value?.close();
					result = await reader.read();
				}
			} catch (error) {
				console.error("Error during frame draining:", error);
			} finally {
				await reader.cancel();
			}
		});

		const decoder = new VideoDecoder({
			output: async (frame: VideoFrame) => {
				// Insert into a queue so we can perform ordered sleeps.
				// If this were to block, I believe WritableStream is still ordered.
				try {
					await writer.write(frame);
				} catch {
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

		// Output processing - same for both container types
		effect.spawn(async () => {
			for (;;) {
				const { value: frame } = await reader.read();
				if (!frame) break;

				// Sleep until it's time to decode the next frame.
				const ref = performance.now() - frame.timestamp / 1000;

				let sleep = 0;
				if (!this.#reference || ref < this.#reference) {
					this.#reference = ref;
					// Don't sleep so we immediately render this frame.
				} else {
					sleep = this.#reference - ref + this.#latency.peek();
				}

				if (sleep > MIN_SYNC_WAIT_MS) {
					this.syncStatus.set({ state: "wait", bufferDuration: sleep });
				}

				if (sleep > 0) {
					// NOTE: WebCodecs doesn't block on output promises (I think?), so these sleeps will occur concurrently.
					// TODO: This cause the `syncStatus` to be racey especially
					await new Promise((resolve) => setTimeout(resolve, sleep));
				}

				if (sleep > MIN_SYNC_WAIT_MS) {
					// Include how long we slept if it was above the threshold.
					this.syncStatus.set({ state: "ready", bufferDuration: sleep });
				} else {
					this.syncStatus.set({ state: "ready" });

					// If the track switch was pending, complete it now.
					if (this.#pending === effect) {
						this.#active?.close();
						this.#active = effect;
						this.#pending = undefined;
						effect.set(this.active, name);
					}
				}

				this.frame.update((prev) => {
					prev?.close();
					return frame;
				});
			}
		});

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
			latency: this.#latency.combined,
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
				const group = await sub.nextGroup();
				if (!group) break;

				effect.spawn(async () => {
					try {
						for (;;) {
							const segment = await group.readFrame();
							if (!segment) break;

							const samples = Container.Cmaf.decodeDataSegment(segment, timescale);

							for (const sample of samples) {
								const chunk = new EncodedVideoChunk({
									type: sample.keyframe ? "key" : "delta",
									data: sample.data,
									timestamp: sample.timestamp,
								});

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

	#selectRendition(renditions: Record<string, Catalog.VideoConfig>, target?: Target): string | undefined {
		const entries = Object.entries(renditions);
		if (entries.length <= 1) return entries.at(0)?.[0];

		// If we have no target, then choose the largest supported rendition.
		// This is kind of a hack to use MAX_SAFE_INTEGER / 2 - 1 but IF IT WORKS, IT WORKS.
		const pixels = target?.pixels ?? Number.MAX_SAFE_INTEGER / 2 - 1;

		// Round up to the closest rendition.
		// Also keep track of the 2nd closest, just in case there's nothing larger.

		let larger: string | undefined;
		let largerSize: number | undefined;

		let smaller: string | undefined;
		let smallerSize: number | undefined;

		for (const [name, rendition] of entries) {
			if (!rendition.codedHeight || !rendition.codedWidth) continue;

			const size = rendition.codedHeight * rendition.codedWidth;
			if (size > pixels && (!largerSize || size < largerSize)) {
				larger = name;
				largerSize = size;
			} else if (size < pixels && (!smallerSize || size > smallerSize)) {
				smaller = name;
				smallerSize = size;
			}
		}
		if (larger) return larger;
		if (smaller) return smaller;

		console.warn("no width/height information, choosing the first supported rendition");
		return entries[0][0];
	}

	#runDisplay(effect: Effect): void {
		const catalog = effect.get(this.catalog);
		if (!catalog) return;

		const display = catalog.display;
		if (display) {
			effect.set(this.display, {
				width: display.width,
				height: display.height,
			});
			return;
		}

		const frame = effect.get(this.frame);
		if (!frame) return;

		effect.set(this.display, {
			width: frame.displayWidth,
			height: frame.displayHeight,
		});
	}

	#runBuffer(effect: Effect): void {
		const frame = effect.get(this.frame);
		const enabled = effect.get(this.enabled);

		const isBufferEmpty = enabled && !frame;
		if (isBufferEmpty) {
			this.bufferStatus.set({ state: "empty" });
		} else {
			this.bufferStatus.set({ state: "filled" });
		}
	}

	close() {
		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});

		this.#latency.close();
		this.#signals.close();
	}
}
