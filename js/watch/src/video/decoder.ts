import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { base64ToBytes } from "../base64";
import type { BufferedRanges } from "../buffered";
import type { Sync } from "../sync";
import type { Backend, Stats } from "./backend";
import type { Source } from "./source";

// The amount of time to wait before considering the video to be buffering.
const BUFFERING = Time.Milli(500);
const SWITCH = Time.Milli(100);

type DecoderInput = {
	// Whether to download the video track. Wired from the renderer's output by the parent.
	enabled: Getter<boolean>;
};

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
	buffered: Signal<BufferedRanges>;
};

// The types in VideoDecoderConfig that cause a hard reload.
// ex. codedWidth/Height are optional and can be changed in-band, so we don't want to trigger a reload.
// This way we can keep the current subscription active.
type RequiredDecoderConfig = Omit<Catalog.VideoConfig, "codedWidth" | "codedHeight">;

export class Decoder implements Backend {
	readonly input: Readonlys<DecoderInput>;
	source: Source;
	sync: Sync;

	readonly #output: DecoderOutput = {
		frame: new Signal<VideoFrame | undefined>(undefined),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		display: new Signal<{ width: number; height: number } | undefined>(undefined),
		stalled: new Signal<boolean>(false),
		stats: new Signal<Stats | undefined>(undefined),
		buffered: new Signal<BufferedRanges>([]),
	};
	readonly output = readonlys(this.#output);

	// The current track running, held so we can cancel it when the new track is ready.
	#active = new Signal<DecoderTrack | undefined>(undefined);

	#signals = new Effect();

	#clearCurrentFrame(): void {
		this.#output.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
		this.#output.timestamp.set(undefined);
	}

	constructor(source: Source, sync: Sync, props?: Inputs<DecoderInput>) {
		this.input = {
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
			this.input.enabled,
			this.source.input.broadcast,
			this.source.output.track,
			this.source.output.config,
		]);
		if (!values) {
			// Close the active track when disabled (e.g. paused or not visible).
			// The pending cleanup won't do this because it was already promoted to #active.
			this.#active.set(undefined);
			return;
		}
		const [_, broadcast, track, config] = values;

		const active: Moq.broadcast.Consumer | undefined = effect.get(broadcast.output.active);
		if (!active) {
			// Going offline should clear the last rendered frame.
			this.#active.set(undefined);
			this.#clearCurrentFrame();
			this.#output.buffered.set([]);
			return;
		}

		// Start a new pending effect.
		let pending: DecoderTrack | undefined = new DecoderTrack({
			sync: this.sync,
			broadcast: active,
			track,
			config,
			sourceTrack: this.source.output.track,
			sourceConfig: this.source.output.config,
			stats: this.#output.stats,
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
			this.#output.buffered.set([]);
			return;
		}

		effect.cleanup(() => active.close());

		// Clone the frame so we own it independently of the DecoderTrack.
		// proxy() would share the same reference, allowing the source to close our frame.
		effect.run((inner) => {
			const frame = inner.get(active.frame);
			this.#output.frame.update((prev) => {
				prev?.close();
				return frame?.clone();
			});
		});
		effect.proxy(this.#output.timestamp, active.timestamp);
		effect.proxy(this.#output.buffered, active.buffered);
	}

	#runDisplay(effect: Effect): void {
		const catalog = effect.get(this.source.output.catalog);
		if (!catalog) return;

		const display = catalog.display;
		if (display) {
			effect.set(this.#output.display, {
				width: display.width,
				height: display.height,
			});
			return;
		}

		const frame = effect.get(this.#output.frame);
		if (!frame) return;

		effect.set(this.#output.display, {
			width: frame.displayWidth,
			height: frame.displayHeight,
		});
	}

	#runBuffering(effect: Effect): void {
		const enabled = effect.get(this.input.enabled);
		if (!enabled) return;

		const frame = effect.get(this.#output.frame);
		if (!frame) {
			this.#output.stalled.set(true);
			return;
		}

		this.#output.stalled.set(false);

		effect.timer(() => {
			this.#output.stalled.set(true);
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
	broadcast: Moq.broadcast.Consumer;
	track: string;
	config: Catalog.VideoConfig;
	// Live source track/config so a running track can detect when the catalog has moved past its frozen
	// config (a codec switch) and stop restarting against wrong-codec bytes.
	sourceTrack: Getter<string | undefined>;
	sourceConfig: Getter<Catalog.VideoConfig | undefined>;

	stats: Signal<Stats | undefined>;
}

// Max in-place decoder rebuilds (see the DecoderTrack error callback) before giving up, so a config that
// configures cleanly but always fails to decode can't restart-loop forever. Reset on a successful decode.
const MAX_DECODER_RESTARTS = 5;

// When a restart errors again within RESTART_RAPID_MS, the fresh subscription immediately hit the same
// wrong-codec bytes (a codec switch whose catalog update still lags the media track). Back off ~one group
// before the next retry so the budget spans the lag window instead of burning out in a sub-second storm.
const RESTART_RAPID_MS = 300;
const RESTART_BACKOFF_MS = 500;

// Cap the encoded frames queued in the hardware decoder. Steady-state live playback keeps the queue near
// zero (frames arrive network-paced); only a subscribe/restart catch-up burst (up to a full GoP, ~120
// frames at 1080p60) exceeds this, and an unbounded burst can overwhelm hardware decoders at high
// resolutions. Backpressure, not dropping: frame order and count are untouched, only the catch-up is paced.
const MAX_DECODE_QUEUE = 32;

// Wait until the decoder's queue drops back to the cap (or the effect tears down). Resolves promptly on
// each `dequeue`; the timer is a fallback for a browser that doesn't fire it (it is spec'd and shipped
// everywhere, but don't hang the decode loop on that). Each wait fully unregisters its own listeners and
// timer so a sustained-overload stream (e.g. 4K the decoder can't keep up with) doesn't accumulate them.
async function drainDecodeQueue(decoder: VideoDecoder, effect: Effect): Promise<void> {
	const signal = effect.abort; // pin the run's signal; the getter is replaced on every re-run
	while (decoder.state === "configured" && decoder.decodeQueueSize > MAX_DECODE_QUEUE) {
		if (signal.aborted) return; // torn down
		await new Promise<void>((resolve) => {
			let timer: ReturnType<typeof setTimeout> | undefined;
			const done = () => {
				if (timer !== undefined) clearTimeout(timer);
				decoder.removeEventListener("dequeue", done);
				signal.removeEventListener("abort", done);
				resolve();
			};
			decoder.addEventListener("dequeue", done, { once: true });
			signal.addEventListener("abort", done, { once: true });
			timer = setTimeout(done, 50);
		});
	}
}

class DecoderTrack {
	sync: Sync;
	broadcast: Moq.broadcast.Consumer;
	track: string;
	config: RequiredDecoderConfig;
	stats: Signal<Stats | undefined>;

	timestamp = new Signal<Time.Milli | undefined>(undefined);
	// The last decoded frame, held ACROSS #restart re-runs: a restart rebuilds the subscription + decoder
	// but must NOT clear this, or the renderer paints black between restarts. Only a real track promotion
	// or close (DecoderTrack.close) clears it.
	frame = new Signal<VideoFrame | undefined>(undefined);

	// Network jitter + decode buffer.
	buffered = new Signal<BufferedRanges>([]);

	// Decoded frames waiting to be rendered.
	#buffered = new Signal<BufferedRanges>([]);

	// The last discontinuity count seen from the container consumer; doubles as a generation
	// so in-flight decodes from before a rewind can be dropped on output.
	#discontinuity = 0;

	// Live source track/config, so #superseded can tell when the catalog has moved past this track's
	// frozen config (a codec switch) and a replacement DecoderTrack is already on the way.
	#sourceTrack: Getter<string | undefined>;
	#sourceConfig: Getter<Catalog.VideoConfig | undefined>;

	// Bumped by the decoder's error callback to rebuild the decoder in place (re-run #run) instead of
	// permanently closing the track. Capped by #restartCount (reset on a successful decode) so a config
	// that configures cleanly but always fails to decode can't loop forever.
	#restart = new Signal(0);
	#restartCount = 0;
	// performance.now() of the last restart, to detect rapid repeats (a config mismatch, not a transient).
	#lastRestart = 0;

	signals = new Effect();

	constructor(props: DecoderTrackProps) {
		// Remove the codedWidth/Height from the config to avoid a hard reload if nothing else has changed.
		const { codedWidth: _, codedHeight: __, ...requiredConfig } = props.config;

		this.sync = props.sync;
		this.broadcast = props.broadcast;
		this.track = props.track;
		this.config = requiredConfig;
		this.#sourceTrack = props.sourceTrack;
		this.#sourceConfig = props.sourceConfig;
		this.stats = props.stats;

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		// Re-run (rebuild subscription + decoder) when the error callback bumps #restart.
		const restarted = effect.get(this.#restart) > 0;

		// A restart re-run whose config the catalog has already moved past would only re-decode wrong-codec
		// bytes until the replacement DecoderTrack promotes. Idle instead, holding the last frame. Never on
		// the first run (restarted === false), which must always proceed.
		if (restarted && this.#superseded()) return;

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

					// A frame rendered, so the decoder is healthy: reset the restart budget.
					this.#restartCount = 0;

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
			error: (error) => {
				// Rebuild the decoder in place rather than permanently closing the track. WebCodecs has
				// already closed the decoder here, so recovery needs a fresh one via a #run re-run.

				// If the catalog has moved past this config (a codec switch whose media bytes arrived before
				// the catalog update), a replacement DecoderTrack is already coming. Stop restarting instead
				// of storming resubscribes against stale wrong-codec bytes, and don't burn the budget on them.
				if (this.#superseded()) {
					console.debug("video decoder error; config superseded, awaiting replacement", error);
					return;
				}

				if (this.#restartCount >= MAX_DECODER_RESTARTS) {
					console.error("video decoder error; restart budget exhausted", error);
					effect.close();
					return;
				}

				// Rapid repeat = the fresh subscription hit the same wrong-codec bytes (catalog lag after a
				// codec switch). Back off ~one group so the budget spans the lag window instead of a
				// sub-second storm; an isolated (transient) error still restarts immediately. "rapid" is
				// measured against when the restart is DISPATCHED (stamped in `restart` below), not the error
				// time, so a backed-off restart doesn't reset the interval and oscillate immediate/backoff.
				const rapid = performance.now() - this.#lastRestart < RESTART_RAPID_MS;

				this.#restartCount++;
				// First restart warns; subsequent ones are debug so a routine codec switch doesn't spam.
				if (this.#restartCount === 1) console.warn("video decoder error; restarting", error);
				else console.debug("video decoder error; restarting", error);

				const restart = () => {
					// Re-check after the backoff timer: the catalog may have landed during the wait.
					if (this.#superseded()) return;
					this.#lastRestart = performance.now();
					this.#restart.update((n) => n + 1);
				};
				if (rapid) effect.timer(restart, RESTART_BACKOFF_MS);
				else restart();
			},
		});
		effect.cleanup(() => {
			if (decoder.state !== "closed") decoder.close();
		});

		// Input processing - depends on container type
		if (this.config.container.kind === "cmaf") {
			this.#runCmaf(effect, sub, decoder, restarted);
		} else {
			this.#runLegacy(effect, sub, decoder, restarted);
		}
	}

	// True when the catalog has moved past this track's frozen config: the Source is already building a
	// replacement DecoderTrack, so restarting this one would only re-decode wrong-codec bytes until the
	// replacement promotes. Compares by track name and config fields, never object identity (Signal.set
	// stores a deep-equal object without notifying, so peek() can return a new-but-equal object).
	#superseded(): boolean {
		if (this.#sourceTrack.peek() !== this.track) return true;
		return configSuperseded(this.#sourceConfig.peek(), this.config);
	}

	#runLegacy(effect: Effect, sub: Moq.track.Subscriber, decoder: VideoDecoder, restarted: boolean): void {
		const format =
			this.config.container.kind === "loc" ? new Container.Loc.Format() : new Container.Legacy.Format();
		// Create consumer that reorders groups/frames up to the provided latency.
		const consumer = new Container.Consumer(sub, {
			format,
			latency: this.sync.output.buffer,
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
		// A restarted run must not feed deltas before its first keyframe. Normally a no-op: a fresh
		// subscribe delivers the live group from frame 0 (forced keyframe), so this clears immediately;
		// it only bites the group-head-eviction / mid-group-start edge.
		let resyncing = restarted;

		effect.spawn(async () => {
			for (;;) {
				const next = await consumer.next();
				if (!next) break;

				// Publisher rewound: flush queued/in-flight video and re-anchor before decoding.
				if (this.#onDiscontinuity(next.discontinuity)) {
					previous = undefined;
					resyncing = false;
				}

				const { frame, group } = next;

				if (!frame) {
					if (previous) {
						previous.final = true;
					}
					// The group is done
					continue;
				}

				// While resyncing after a decode error, wait for the next keyframe (group start) before
				// decoding again; group index 0 is always a keyframe, so this resyncs within one group.
				if (resyncing) {
					if (!frame.keyframe) continue;
					resyncing = false;
				}

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
				this.sync.received(timestamp, "video");

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

				if (decoder.decodeQueueSize > MAX_DECODE_QUEUE) await drainDecodeQueue(decoder, effect);
				if (decoder.state === "closed") break;
				try {
					decoder.decode(chunk);
				} catch (err) {
					// Wrong-codec bytes (e.g. a mid-stream codec switch landing on the tail of the old
					// codec's group) make decode() throw DataError synchronously. Skip to the next keyframe
					// group and resync there instead of letting this spawn die and freezing until the next
					// catalog round-trip. Any other error (closed/invalid state) stops the loop.
					if (err instanceof DOMException && err.name === "DataError") {
						console.debug("video decode error; resyncing at next keyframe", err);
						resyncing = true;
						previous = undefined;
					} else {
						break;
					}
				}
			}
		});
	}

	#runCmaf(effect: Effect, sub: Moq.track.Subscriber, decoder: VideoDecoder, restarted: boolean): void {
		if (this.config.container.kind !== "cmaf") return;

		const initSegment = base64ToBytes(this.config.container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		const description = this.config.description ? Util.Hex.toBytes(this.config.description) : init.description;

		const consumer = new Container.Consumer(sub, {
			format: new Container.Cmaf.Format(init),
			latency: this.sync.output.buffer,
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
		// A restarted run must not feed deltas before its first keyframe. Normally a no-op: a fresh
		// subscribe delivers the live group from frame 0 (forced keyframe), so this clears immediately;
		// it only bites the group-head-eviction / mid-group-start edge.
		let resyncing = restarted;

		effect.spawn(async () => {
			for (;;) {
				const next = await consumer.next();
				if (!next) break;

				// Publisher rewound: flush queued/in-flight video and re-anchor before decoding.
				if (this.#onDiscontinuity(next.discontinuity)) {
					previous = undefined;
					resyncing = false;
				}

				const { frame, group } = next;

				if (!frame) {
					if (previous) {
						previous.final = true;
					}
					continue;
				}

				// While resyncing after a decode error, wait for the next keyframe (group start) before
				// decoding again; group index 0 is always a keyframe, so this resyncs within one group.
				if (resyncing) {
					if (!frame.keyframe) continue;
					resyncing = false;
				}

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp);
				this.sync.received(timestamp, "video");

				// Track stats
				this.stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					bytesReceived: (current?.bytesReceived ?? 0) + frame.data.byteLength,
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

				if (decoder.decodeQueueSize > MAX_DECODE_QUEUE) await drainDecodeQueue(decoder, effect);
				if (decoder.state === "closed") break;
				try {
					decoder.decode(
						new EncodedVideoChunk({
							type: frame.keyframe ? "key" : "delta",
							data: frame.data,
							timestamp: frame.timestamp,
						}),
					);
				} catch (err) {
					// See #runLegacy: skip to the next keyframe on a wrong-codec DataError instead of dying.
					if (err instanceof DOMException && err.name === "DataError") {
						console.debug("video decode error; resyncing at next keyframe", err);
						resyncing = true;
						previous = undefined;
					} else {
						break;
					}
				}
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
		this.signals.close();

		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

/**
 * Whether a live catalog config has diverged from the one a decoder was frozen with, in a way that needs
 * a new decoder (codec, container kind, or codec description). Compared by value, never object identity,
 * because Signal.set stores a deep-equal object without notifying. Exported for unit tests.
 */
export function configSuperseded(
	current: Pick<Catalog.VideoConfig, "codec" | "container" | "description"> | undefined,
	frozen: Pick<Catalog.VideoConfig, "codec" | "container" | "description">,
): boolean {
	if (!current) return false;
	return (
		current.codec !== frozen.codec ||
		current.container.kind !== frozen.container.kind ||
		current.description !== frozen.description
	);
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
