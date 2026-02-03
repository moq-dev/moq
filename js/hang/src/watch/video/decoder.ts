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
const SWITCH = 100 as Time.Milli;

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
	#active = new Signal<DecoderTrack | undefined>(undefined);

	// Expose the current frame to render as a signal
	#frame = new Signal<VideoFrame | undefined>(undefined);
	readonly frame: Getter<VideoFrame | undefined> = this.#frame;

	// The timestamp of the current frame.
	#timestamp = new Signal<Time.Milli | undefined>(undefined);
	readonly timestamp: Getter<Time.Milli | undefined> = this.#timestamp;

	// The display size of the video in pixels, ideally sourced from the catalog.
	#display = new Signal<{ width: number; height: number } | undefined>(undefined);
	readonly display: Getter<{ width: number; height: number } | undefined> = this.#display;

	#stalled = new Signal<boolean>(false);
	readonly stalled: Getter<boolean> = this.#stalled;

	#stats = new Signal<Stats | undefined>(undefined);
	readonly stats: Getter<Stats | undefined> = this.#stats;

	// Combined buffered ranges (network jitter + decode buffer)
	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	constructor(source: Source, props?: DecoderProps) {
		this.enabled = Signal.from(props?.enabled ?? false);

		this.source = source;
		this.source.supported.set(supported); // super hacky

		this.#signals.effect(this.#runPending.bind(this));
		this.#signals.effect(this.#runActive.bind(this));
		this.#signals.effect(this.#runDisplay.bind(this));
		this.#signals.effect(this.#runBuffering.bind(this));
	}

	#runPending(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.source.broadcast, this.source.track, this.source.config]);
		if (!values) return;
		const [_, source, track, config] = values;

		const broadcast = effect.get(source.active);
		if (!broadcast) return;

		// Start a new pending effect.
		let pending: DecoderTrack | undefined = new DecoderTrack({
			source: this.source,
			broadcast,
			track,
			config,
			stats: this.#stats,
		});

		effect.cleanup(() => pending?.close());

		effect.effect((effect) => {
			if (!pending) return;

			const active = effect.get(this.#active);
			if (active) {
				const pendingTimestamp = effect.get(pending.timestamp);
				const activeTimestamp = effect.get(active.timestamp);

				// Switch to the new track if it's ready and we've caught up enough.
				if (!pendingTimestamp) return;
				if (activeTimestamp && activeTimestamp > pendingTimestamp + SWITCH) return;
			}

			// Upgrade the pending track to active.
			// #runActive will be in charge of it now.
			this.#active.set(pending);
			pending = undefined;
		});
	}

	#runActive(effect: Effect): void {
		const active = effect.get(this.#active);
		if (!active) return;

		effect.cleanup(() => active.close());

		effect.proxy(this.#frame, active.frame);
		effect.proxy(this.#timestamp, active.timestamp);
		effect.proxy(this.#buffered, active.buffered);
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
			this.#stalled.set(true);
			return;
		}

		this.#stalled.set(false);

		effect.timer(() => {
			this.#stalled.set(true);
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

interface DecoderTrackProps {
	source: Source;
	broadcast: Moq.Broadcast;
	track: string;
	config: Catalog.VideoConfig;

	stats: Signal<Stats | undefined>;
}

class DecoderTrack {
	source: Source;
	broadcast: Moq.Broadcast;
	track: string;
	config: RequiredDecoderConfig;
	stats: Signal<Stats | undefined>;

	timestamp = new Signal<Time.Milli | undefined>(undefined);
	frame = new Signal<VideoFrame | undefined>(undefined);

	// Network jitter + decode buffer.
	buffered = new Signal<BufferedRanges>([]);

	// Decoded frames waiting to be rendered.
	#buffered = new Signal<BufferedRanges>([]);

	signals = new Effect();

	constructor(props: DecoderTrackProps) {
		// Remove the codedWidth/Height from the config to avoid a hard reload if nothing else has changed.
		const { codedWidth: _, codedHeight: __, ...requiredConfig } = props.config;

		this.source = props.source;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.config = requiredConfig;
		this.stats = props.stats;

		this.signals.effect(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const sub = this.broadcast.subscribe(this.track, Catalog.PRIORITY.video);
		effect.cleanup(() => sub.close());

		const decoder = new VideoDecoder({
			output: async (frame: VideoFrame) => {
				try {
					const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						return;
					}

					if (this.frame.peek() === undefined) {
						// Render something while we wait for the sync to catch up.
						this.frame.set(frame.clone());
					}

					const wait = this.source.sync.wait(timestamp).then(() => true);
					const ok = await Promise.race([wait, effect.cancel]);
					if (!ok) return;

					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						// NOTE: This can happen when the ref is updated, such as on playback start.
						return;
					}

					this.timestamp.set(timestamp);

					// Trim the decode buffer as frames are rendered
					this.#trimBuffered(timestamp);

					this.frame.update((prev) => {
						prev?.close();
						return frame.clone(); // avoid closing the frame here
					});
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
		if (this.config.container.kind === "cmaf") {
			this.#runCmaf(effect, sub, decoder);
		} else {
			this.#runLegacy(effect, sub, decoder);
		}
	}

	#runLegacy(effect: Effect, sub: Moq.Track, decoder: VideoDecoder): void {
		// Create consumer that reorders groups/frames up to the provided latency.
		const consumer = new Container.Legacy.Consumer(sub, {
			latency: this.source.sync.latency,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.effect((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#buffered);
			this.buffered.update(() => mergeBufferedRanges(network, decode));
		});

		decoder.configure({
			...this.config,
			description: this.config.description ? Hex.toBytes(this.config.description) : undefined,
			optimizeForLatency: this.config.optimizeForLatency ?? true,
			// @ts-expect-error Only supported by Chrome, so the renderer has to flip manually.
			flip: false,
		});

		let previous: { timestamp: Time.Micro; group: number; final: boolean } | undefined;

		effect.spawn(async () => {
			for (;;) {
				const next = await Promise.race([consumer.next(), effect.cancel]);
				if (!next) break;

				const { frame, group } = next;

				if (!frame) {
					if (previous) {
						previous.final = true;
					}
					// The group is done
					continue;
				}

				// Mark that we received this frame right now.
				this.source.sync.received(Time.Milli.fromMicro(frame.timestamp as Time.Micro));

				const chunk = new EncodedVideoChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.data,
					timestamp: frame.timestamp,
				});

				// Track both frame count and bytes received for stats in the UI
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					bytesReceived: (current?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				// Track decode buffer: frames sent to decoder but not yet rendered
				if (previous?.group === group || (previous?.final && previous.group + 1 === group)) {
					const start = Time.Milli.fromMicro(previous.timestamp);
					const end = Time.Milli.fromMicro(frame.timestamp);
					this.#addBuffered(start, end);
				}

				previous = {
					timestamp: frame.timestamp,
					group,
					final: false,
				};

				decoder.decode(chunk);
			}
		});
	}

	#runCmaf(effect: Effect, sub: Moq.Track, decoder: VideoDecoder): void {
		if (this.config.container.kind !== "cmaf") return;

		const { timescale } = this.config.container;
		const description = this.config.description ? Hex.toBytes(this.config.description) : undefined;

		// Configure decoder with description from catalog
		decoder.configure({
			codec: this.config.codec,
			description,
			optimizeForLatency: this.config.optimizeForLatency ?? true,
			// @ts-expect-error Only supported by Chrome, so the renderer has to flip manually.
			flip: false,
		});

		// Use decode buffer directly (no network jitter buffer for CMAF yet)
		effect.effect((inner) => {
			const decode = inner.get(this.#buffered);
			this.buffered.update(() => decode);
		});

		effect.spawn(async () => {
			// Process data segments
			// TODO: Use a consumer wrapper for CMAF to support latency control
			for (;;) {
				const group = await Promise.race([sub.nextGroup(), effect.cancel]);
				if (!group) break;

				effect.spawn(async () => {
					let previous: Time.Micro | undefined;

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
								this.stats.update((current) => ({
									frameCount: (current?.frameCount ?? 0) + 1,
									bytesReceived: (current?.bytesReceived ?? 0) + sample.data.byteLength,
								}));

								// Track decode buffer
								if (previous !== undefined) {
									const start = Time.Milli.fromMicro(previous);
									const end = Time.Milli.fromMicro(sample.timestamp as Time.Micro);
									this.#addBuffered(start, end);
								}
								previous = sample.timestamp as Time.Micro;

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

	// Add a range to the decode buffer (decoded, waiting to render)
	#addBuffered(start: Time.Milli, end: Time.Milli): void {
		if (start > end) return;

		this.#buffered.mutate((current) => {
			for (const range of current) {
				// Check if there's any overlap, then merge
				if (range.start <= end && range.end >= start) {
					range.start = Time.Milli.min(range.start, start);
					range.end = Time.Milli.max(range.end, end);
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start - b.start);
		});
	}

	// Trim the decode buffer up to the rendered timestamp
	#trimBuffered(timestamp: Time.Milli): void {
		this.#buffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end >= timestamp) {
					current[0].start = Time.Milli.max(current[0].start, timestamp);
					break;
				}
				current.shift();
			}
		});
	}

	close(): void {
		this.signals.close();

		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

// Merge two sets of buffered ranges into one sorted list
function mergeBufferedRanges(a: BufferedRanges, b: BufferedRanges): BufferedRanges {
	if (a.length === 0) return b;
	if (b.length === 0) return a;

	const result: BufferedRanges = [];
	const all = [...a, ...b].sort((x, y) => x.start - y.start);

	for (const range of all) {
		const last = result.at(-1);
		if (last && last.end >= range.start) {
			// Merge overlapping ranges
			last.end = Time.Milli.max(last.end, range.end);
		} else {
			result.push({ ...range });
		}
	}

	return result;
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
