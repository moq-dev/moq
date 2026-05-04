import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/lite";
import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { BufferedRanges } from "../backend";
import { base64ToBytes } from "../base64";
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

		this.#signals.run(this.#runPending.bind(this));
		this.#signals.run(this.#runActive.bind(this));
		this.#signals.run(this.#runDisplay.bind(this));
		this.#signals.run(this.#runBuffering.bind(this));
	}

	#runPending(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.source.broadcast, this.source.track, this.source.config]);
		if (!values) {
			// Close the active track when disabled (e.g. paused or not visible).
			// The pending cleanup won't do this because it was already promoted to #active.
			this.#active.set(undefined);
			return;
		}
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

		effect.run((effect) => {
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

			// This effect is done; close it to avoid a useless re-run.
			effect.close();
		});
	}

	#runActive(effect: Effect): void {
		const active = effect.get(this.#active);
		if (!active) {
			// Clear stale data when disabled (e.g. paused or not visible).
			this.#buffered.set([]);
			return;
		}

		effect.cleanup(() => active.close());

		// Clone the frame so we own it independently of the DecoderTrack.
		// proxy() would share the same reference, allowing the source to close our frame.
		effect.run((inner) => {
			const frame = inner.get(active.frame);
			this.#frame.update((prev) => {
				prev?.close();
				return frame?.clone();
			});
		});
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

		this.signals.run(this.#run.bind(this));
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
		effect.cleanup(() => {
			if (decoder.state !== "closed") decoder.close();
		});

		// Input processing - depends on container type
		if (this.config.container.kind === "cmaf") {
			this.#runCmaf(effect, sub, decoder);
		} else {
			this.#runLegacy(effect, sub, decoder);
		}
	}

	#runLegacy(effect: Effect, sub: Moq.Track, decoder: VideoDecoder): void {
		// Create consumer that reorders groups/frames up to the provided latency.
		const consumer = new Container.Consumer(sub, {
			format: new Container.Legacy.Format(),
			latency: this.source.sync.buffer,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#buffered);
			this.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		decoder.configure({
			...this.config,
			description: this.config.description ? Util.Hex.toBytes(this.config.description) : undefined,
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
				const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
				this.source.sync.received(timestamp, "video");

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

		const initSegment = base64ToBytes(this.config.container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		const description = this.config.description ? Util.Hex.toBytes(this.config.description) : undefined;

		const consumer = new Container.Consumer(sub, {
			format: new Container.Cmaf.Format(init),
			latency: this.source.sync.buffer,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#buffered);
			this.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		// Configure decoder with description from catalog
		decoder.configure({
			codec: this.config.codec,
			description,
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
					continue;
				}

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp);
				this.source.sync.received(timestamp, "video");

				// Track stats
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					bytesReceived: (current?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				// Track decode buffer
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

				if (decoder.state === "closed") break;
				decoder.decode(
					new EncodedVideoChunk({
						type: frame.keyframe ? "key" : "delta",
						data: frame.data,
						timestamp: frame.timestamp,
					}),
				);
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

async function supported(config: Catalog.VideoConfig): Promise<boolean> {
	const description = config.description ? Util.Hex.toBytes(config.description) : undefined;
	const { supported } = await VideoDecoder.isConfigSupported({
		codec: config.codec,
		description,
		optimizeForLatency: config.optimizeForLatency ?? true,
	});

	if (supported) return true;

	// Safari rejects `avc3.*` codec strings even though its H.264 decoder handles
	// inline SPS/PPS. Rewrite to `avc1.*` and retry; mutate config.codec so the
	// later `decoder.configure()` call uses the accepted string too.
	if (config.codec.startsWith("avc3.")) {
		const avc1 = `avc1.${config.codec.slice("avc3.".length)}`;
		const retry = await VideoDecoder.isConfigSupported({
			codec: avc1,
			description,
			optimizeForLatency: config.optimizeForLatency ?? true,
		});
		if (retry.supported) {
			config.codec = avc1;
			return true;
		}
	}

	return false;
}
