import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { base64ToBytes } from "../base64";

import type { Sync } from "../sync";
import type { Source } from "./source";

// The amount of time to wait before considering the video to be buffering.
const BUFFERING = Time.Milli(500);
const SWITCH = Time.Milli(100);

export type DecoderInput = {
	// Whether to download the video track. Wired from the renderer's output by the parent.
	enabled: Getter<boolean>;
};

/** Cumulative video statistics since the decoder started. */
export interface Stats {
	/** Number of decoded frames. */
	frameCount: number;

	/** Number of encoded bytes received. */
	bytesReceived: number;
}

type DecoderOutput = {
	// The current frame to render.
	frame: Signal<VideoFrame | undefined>;

	// The timestamp of the current frame.
	timestamp: Signal<Time.Milli | undefined>;

	// The display size of the video in pixels, ideally sourced from the catalog.
	display: Signal<{ width: number; height: number } | undefined>;

	stalled: Signal<boolean>;
	stats: Signal<Stats | undefined>;

	// Combined buffered ranges (network jitter + decode buffer)
	buffered: Signal<Container.BufferedRanges>;
};

// The types in VideoDecoderConfig that cause a hard reload.
// ex. codedWidth/Height are optional and can be changed in-band, so we don't want to trigger a reload.
// This way we can keep the current subscription active.
type RequiredDecoderConfig = Omit<Catalog.VideoConfig, "codedWidth" | "codedHeight">;

/** Downloads video from a track and decodes it into {@link VideoFrame}s with WebCodecs. */
export class Decoder {
	readonly in: Readonlys<DecoderInput>;
	readonly source: Source;
	readonly sync: Sync;

	readonly #out: DecoderOutput = {
		frame: new Signal<VideoFrame | undefined>(undefined),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		display: new Signal<{ width: number; height: number } | undefined>(undefined),
		stalled: new Signal<boolean>(false),
		stats: new Signal<Stats | undefined>(undefined),
		buffered: new Signal<Container.BufferedRanges>([]),
	};
	readonly out = readonlys(this.#out);

	// The current track running, held so we can cancel it when the new track is ready.
	#active = new Signal<DecoderTrack | undefined>(undefined);

	#signals = new Effect();

	#clearCurrentFrame(): void {
		this.#out.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
		this.#out.timestamp.set(undefined);
	}

	constructor(source: Source, sync: Sync, props?: Inputs<DecoderInput>) {
		this.in = {
			enabled: getter(props?.enabled ?? false),
		};

		this.source = source;
		this.sync = sync;

		this.#signals.run(this.#runPending.bind(this));
		this.#signals.run(this.#runActive.bind(this));
		this.#signals.run(this.#runDisplay.bind(this));
		this.#signals.run(this.#runBuffering.bind(this));
	}

	#runPending(effect: Effect): void {
		const values = effect.getAll([
			this.in.enabled,
			this.source.in.broadcast,
			this.source.out.track,
			this.source.out.config,
		]);
		if (!values) {
			// Close the active track when disabled (e.g. paused or not visible).
			// The pending cleanup won't do this because it was already promoted to #active.
			this.#active.set(undefined);
			return;
		}
		const [_, broadcast, track, config] = values;

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast.
		const active: Moq.Broadcast.Consumer | undefined = broadcast.relativeBroadcast(effect, config.broadcast);
		if (!active) {
			// Going offline should clear the last rendered frame.
			this.#active.set(undefined);
			this.#clearCurrentFrame();
			this.#out.buffered.set([]);
			return;
		}

		// Start a new pending effect.
		let pending: DecoderTrack | undefined = new DecoderTrack({
			sync: this.sync,
			broadcast: active,
			track,
			config,
			stats: this.#out.stats,
		});

		effect.cleanup(() => pending?.close());

		effect.run((effect) => {
			if (!pending) return;

			const current = effect.get(this.#active);
			if (current) {
				const pendingTimestamp = effect.get(pending.timestamp);
				const activeTimestamp = effect.get(current.timestamp);

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
			this.#out.buffered.set([]);
			return;
		}

		effect.cleanup(() => active.close());

		// Clone the frame so we own it independently of the DecoderTrack.
		// proxy() would share the same reference, allowing the source to close our frame.
		effect.run((inner) => {
			const frame = inner.get(active.frame);
			this.#out.frame.update((prev) => {
				prev?.close();
				return frame?.clone();
			});
		});
		effect.proxy(this.#out.timestamp, active.timestamp);
		effect.proxy(this.#out.buffered, active.buffered);
	}

	#runDisplay(effect: Effect): void {
		const catalog = effect.get(this.source.out.catalog);
		if (!catalog) return;

		const display = catalog.display;
		if (display) {
			effect.set(this.#out.display, {
				width: display.width,
				height: display.height,
			});
			return;
		}

		const frame = effect.get(this.#out.frame);
		if (!frame) return;

		effect.set(this.#out.display, {
			width: frame.displayWidth,
			height: frame.displayHeight,
		});
	}

	#runBuffering(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const frame = effect.get(this.#out.frame);
		if (!frame) {
			this.#out.stalled.set(true);
			return;
		}

		this.#out.stalled.set(false);

		effect.timer(() => {
			this.#out.stalled.set(true);
		}, BUFFERING);
	}

	close() {
		this.#clearCurrentFrame();

		this.#signals.close();
	}

	// Whether the WebCodecs video decoder can play this config.
	static supported = supported;
}

interface DecoderTrackProps {
	sync: Sync;
	broadcast: Moq.Broadcast.Consumer;
	track: string;
	config: Catalog.VideoConfig;

	stats: Signal<Stats | undefined>;
}

class DecoderTrack {
	sync: Sync;
	broadcast: Moq.Broadcast.Consumer;
	track: string;
	config: RequiredDecoderConfig;
	stats: Signal<Stats | undefined>;

	timestamp = new Signal<Time.Milli | undefined>(undefined);
	frame = new Signal<VideoFrame | undefined>(undefined);

	// Network jitter + decode buffer.
	buffered = new Signal<Container.BufferedRanges>([]);

	// Decoded frames waiting to be rendered.
	#buffered = new Signal<Container.BufferedRanges>([]);

	// The last discontinuity count seen from the container consumer; doubles as a generation
	// so in-flight decodes from before a rewind can be dropped on output.
	#discontinuity = 0;

	#signals = new Effect();

	constructor(props: DecoderTrackProps) {
		// Remove the codedWidth/Height from the config to avoid a hard reload if nothing else has changed.
		const { codedWidth: _, codedHeight: __, ...requiredConfig } = props.config;

		this.sync = props.sync;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.config = requiredConfig;
		this.stats = props.stats;

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const sub = this.broadcast.track(this.track).subscribe({ priority: Catalog.PRIORITY.video });
		effect.cleanup(() => sub.close());

		const decoder = new VideoDecoder({
			output: async (frame: VideoFrame) => {
				try {
					// The generation this frame was decoded in. If a rewind bumps it while we wait
					// below, this frame belongs to the reneged timeline and must be dropped.
					const generation = this.#discontinuity;

					const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
					if (timestamp < (this.timestamp.peek() ?? 0)) {
						// Late frame, don't render it.
						return;
					}

					if (this.frame.peek() === undefined) {
						// Render something while we wait for the sync to catch up.
						this.frame.set(frame.clone());
					}

					const wait = this.sync.wait(timestamp).then(() => true);
					const ok = await Promise.race([wait, effect.cancel]);
					if (!ok) return;
					if (generation !== this.#discontinuity) return; // a rewind happened while waiting

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

	#runLegacy(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): void {
		const format =
			this.config.container.kind === "loc" ? new Container.Loc.Format() : new Container.Legacy.Format();
		// Create consumer that reorders groups/frames up to the provided latency.
		const consumer = new Container.Consumer(sub, {
			format,
			latency: this.sync.out.buffer,
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
				const next = await consumer.next();
				if (!next) break;

				// Publisher rewound: flush queued/in-flight video and re-anchor before decoding.
				if (this.#onDiscontinuity(next.discontinuity)) previous = undefined;

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
				this.sync.received(timestamp, "video");

				const chunk = new EncodedVideoChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.payload,
					timestamp: frame.timestamp,
				});

				// Track both frame count and bytes received for stats in the UI
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					bytesReceived: (current?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));

				// Track decode buffer: frames sent to decoder but not yet rendered
				const prior = previous;
				if (prior && (prior.group === group || (prior.final && prior.group + 1 === group))) {
					const start = Time.Milli.fromMicro(prior.timestamp);
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

	#runCmaf(effect: Effect, sub: Moq.Track.Subscriber, decoder: VideoDecoder): void {
		if (this.config.container.kind !== "cmaf") return;

		const initSegment = base64ToBytes(this.config.container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		const description = this.config.description ? Util.Hex.toBytes(this.config.description) : init.description;

		const consumer = new Container.Consumer(sub, {
			format: new Container.Cmaf.Format(init),
			latency: this.sync.out.buffer,
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
				const next = await consumer.next();
				if (!next) break;

				// Publisher rewound: flush queued/in-flight video and re-anchor before decoding.
				if (this.#onDiscontinuity(next.discontinuity)) previous = undefined;

				const { frame, group } = next;

				if (!frame) {
					if (previous) {
						previous.final = true;
					}
					continue;
				}

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp);
				this.sync.received(timestamp, "video");

				// Track stats
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					bytesReceived: (current?.bytesReceived ?? 0) + frame.payload.byteLength,
				}));

				// Track decode buffer
				const prior = previous;
				if (prior && (prior.group === group || (prior.final && prior.group + 1 === group))) {
					const start = Time.Milli.fromMicro(prior.timestamp);
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
						data: frame.payload,
						timestamp: frame.timestamp,
					}),
				);
			}
		});
	}

	// React to the container consumer's discontinuity counter. On a change the publisher has
	// rewound the timeline, so drop what's queued downstream and re-anchor the shared clock
	// before the new utterance. Clearing `timestamp` is load-bearing: otherwise its stale high
	// value would late-reject the rewound (lower-timestamp) frames at the output guard. Bumping
	// the generation drops in-flight decodes on output. The held frame is left in place so the
	// last picture shows until the new keyframe renders, instead of flashing empty. Returns true
	// if a rewind was handled.
	#onDiscontinuity(count: number): boolean {
		if (count === this.#discontinuity) return false;
		this.#discontinuity = count;
		this.timestamp.set(undefined);
		this.#buffered.set([]);
		this.sync.reset();
		return true;
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
		this.#signals.close();

		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

async function supported(config: Catalog.VideoConfig): Promise<boolean> {
	let description: Uint8Array | undefined;
	if (config.description) {
		description = Util.Hex.toBytes(config.description);
	} else if (config.container.kind === "cmaf") {
		try {
			description = Container.Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
		} catch (err) {
			// A malformed init segment means we can't extract the codec
			// description, so we can't probe support reliably. Reject the
			// track rather than letting isConfigSupported pass on a
			// description-less config and then having runCmaf fail later.
			console.warn(`video: malformed CMAF init segment for codec ${config.codec}`, err);
			return false;
		}
	}
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
