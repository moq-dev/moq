import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Util from "@moq/hang/util";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { base64ToBytes } from "../base64";
import type { BufferedRanges } from "../buffered";
import type { Sync } from "../sync";
import { type AudioBuffer, createAudioBuffer } from "./buffer";
// Compiled and inlined as a blob URL via vite-plugin-worklet.
import RenderWorklet from "./render-worklet.ts?worklet";
import { snapTimestamp } from "./snap";
import type { Source } from "./source";

type DecoderInput = {
	// Enable to download the audio track.
	enabled: Getter<boolean>;
};

type DecoderOutput = {
	context: Signal<AudioContext | undefined>;

	// The root of the audio graph, which can be used for custom visualizations.
	// Downcast to AudioNode so it matches Publish.Audio
	root: Signal<AudioNode | undefined>;

	sampleRate: Signal<number | undefined>;
	stats: Signal<AudioStats | undefined>;

	// Current playback timestamp from worklet
	timestamp: Signal<Time.Milli | undefined>;

	// Whether the audio buffer is stalled (waiting to fill)
	stalled: Signal<boolean>;

	// Combined buffered ranges (network jitter + decode buffer)
	buffered: Signal<BufferedRanges>;
};

// Opus decoders always emit 48 kHz PCM (RFC 6716) no matter what rate the catalog advertises, so
// build the render pipeline at that rate up front instead of discovering it on the first frame.
const OPUS_OUTPUT_RATE = 48000;

// Decoder restart policy (mirrors js/watch/src/video/decoder.ts). Cap in-place rebuilds so a permanently
// bad config can't loop forever (reset on a successful decode); rapid repeats back off ~one frame.
const MAX_AUDIO_RESTARTS = 5;
const RESTART_RAPID_MS = 300;
const RESTART_BACKOFF_MS = 500;

// Snap a decoded frame's timestamp to the previous frame's exact end when they're within this window, so
// the timestamp-indexed ring writes back-to-back. Comfortably above the worst-case publisher timestamp
// quantization (~one capture quantum, ~2.9 ms) and well below any genuine gap (>= one 20 ms Opus frame:
// packet loss, DTX silence, publisher restart), which must still zero-fill.
const SNAP_US = 5000;

// A wire timestamp jump larger than this is a publisher mute/pause, not jitter: re-anchor playback to
// live instead of playing out the silent gap. A constant (never maxBuffer-scaled) that clears Opus DTX
// comfort-noise spacing (~400 ms) and can never trip on a 20 ms cadence or on packet loss (loss does
// not gap PTS).
const REANCHOR_GAP_US = 1_000_000;

export interface AudioStats {
	/** Number of encoded bytes received. */
	bytesReceived: number;
}

/**
 * Downloads audio from a track and emits it to an AudioContext.
 *
 * The user is responsible for hooking up audio to speakers, an analyzer, etc.
 */
export class Decoder {
	readonly input: Readonlys<DecoderInput>;
	source: Source;
	sync: Sync;

	readonly #output: DecoderOutput = {
		context: new Signal<AudioContext | undefined>(undefined),
		root: new Signal<AudioNode | undefined>(undefined),
		sampleRate: new Signal<number | undefined>(undefined),
		stats: new Signal<AudioStats | undefined>(undefined),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		stalled: new Signal<boolean>(true),
		buffered: new Signal<BufferedRanges>([]),
	};
	readonly output = readonlys(this.#output);

	// The decoder's real output rate paired with the config it was measured against. #emit sets this
	// on a mismatch; #runWorklet then rebuilds the AudioContext/worklet/ring at the true rate. The
	// config tag means a rate measured for one stream is never applied to the next (which would build
	// one wrong-rate pipeline before self-healing on the following frame).
	#decodedRate = new Signal<{ config: Catalog.AudioConfig; rate: number } | undefined>(undefined);

	// The rate the decoder is expected to output (the pipeline's SOURCE rate), set when the ring is built.
	// #emit resamples from this to the ring's actual context rate, and treats a sample at a DIFFERENT rate
	// as a real decoder-rate change (-> #decodedRate rebuild) rather than something to resample.
	#ringSourceRate: number | undefined;

	// Expected timestamp (us) of the next decoded frame. Every frame is snapped onto it when it lands
	// within the window (see SNAP_US), which is an identity no-op on an already-contiguous stream. Reset
	// at every re-anchor (discontinuity, reset, ring rebuild).
	#expectedNext: Time.Micro | undefined;

	// Decoder restart bookkeeping (see #onDecoderError), mirroring the video decoder.
	#restart = new Signal(0);
	#restartCount = 0;
	#lastRestart = 0;
	// The config the current restart budget applies to; a rebuild for a different config resets the budget.
	#budgetConfig: Catalog.AudioConfig | undefined;

	// Decode buffer: audio sent to worklet but not yet played
	#decodeBuffered = new Signal<BufferedRanges>([]);

	// Audio ring bridging main thread and worklet (shared memory or postMessage transport).
	#ring: AudioBuffer | undefined;

	// The last discontinuity count seen from the container consumer. A change means the
	// publisher rewound the timeline (e.g. a voice agent interrupted) and we must flush.
	#discontinuity = 0;

	// How much buffered audio the container consumer retains before skipping
	// ahead. This must be the latency CEILING (maxBuffer), not the floor
	// (buffer): in buffered playback the producer writes faster than real-time
	// with future PTS, so the group span legitimately exceeds the floor and
	// would otherwise be skipped. When collapsed, maxBuffer equals the floor.
	//
	// Held in a plain Signal driven by a running effect (below) rather than a
	// lazy `computed`: the container consumer only `.peek()`s this (it never
	// subscribes), and an unsubscribed computed peeks as `undefined`, which
	// would make the consumer's threshold NaN and skip every group.
	#consumerLatency = new Signal<Time.Milli>(Time.Milli.zero);

	#signals = new Effect();

	constructor(source: Source, sync: Sync, props?: Inputs<DecoderInput>) {
		this.input = {
			enabled: getter(props?.enabled ?? false),
		};

		this.source = source;
		this.sync = sync;

		this.#signals.run((effect) => {
			this.#consumerLatency.set(effect.get(this.sync.output.maxBuffer));
		});

		this.#signals.run(this.#runWorklet.bind(this));
		this.#signals.run(this.#runLatency.bind(this));
		this.#signals.run(this.#runDecoder.bind(this));
	}

	#runWorklet(effect: Effect): void {
		// It takes a second or so to initialize the AudioContext/AudioWorklet, so do it even if disabled.
		// This is less efficient for video-only playback but makes muting/unmuting instant.
		// NOTE: You should disconnect/reconnect the worklet to save power when disabled.

		//const enabled = effect.get(this.enabled);
		//if (!enabled) return;

		const config = effect.get(this.source.output.config);
		if (!config) return;

		// The pipeline must run at the rate the decoder actually OUTPUTS, which is not always the
		// catalog rate: Opus always emits 48 kHz, and #emit corrects any other divergence after the
		// first decoded frame (e.g. a catalog written from a 16 kHz capture context). A rate measured
		// against a different config is ignored, so switching streams starts from the catalog rate.
		const measured = effect.get(this.#decodedRate);
		const decodedRate = measured?.config === config ? measured.rate : undefined;
		const sampleRate = decodedRate ?? (config.codec === "opus" ? OPUS_OUTPUT_RATE : config.sampleRate);
		const channelCount = config.numberOfChannels;

		const context = new AudioContext({
			latencyHint: "interactive", // We don't use real-time because of the buffer.
			sampleRate,
		});
		effect.set(this.#output.context, context);

		effect.cleanup(() => context.close());

		// Safari (and possibly others) ignore the requested sampleRate and run the context at the hardware
		// rate. The worklet + ring run at the context's ACTUAL rate; #emit resamples the decoder's output
		// (sampleRate) to it. `sampleRate` stays the SOURCE rate that #emit compares against to tell a real
		// decoder-rate change apart from this deliberate resample.
		const ringRate = context.sampleRate;
		if (ringRate !== sampleRate) {
			console.debug(`audio: requested ${sampleRate} Hz context, got ${ringRate} Hz; resampling to match`);
		}

		// Safari (and Chrome's autoplay policy) create the AudioContext suspended and only resume it
		// inside a user gesture; a reactive resume() is ignored. Resume on the first interaction, from
		// context creation (not gated on playback). The Emitter (re)builds the graph once the context
		// actually reaches "running" (see emitter.ts), which is what makes Safari start rendering.
		const resume = () => {
			if (context.state === "suspended") void context.resume().catch(() => {});
		};
		resume();
		// pointerdown/keydown cover desktop; iOS Safari has historically only unlocked audio on a
		// click-class gesture (fired at touchend), so listen for that too. resume() is idempotent.
		effect.event(document, "pointerdown", resume);
		effect.event(document, "click", resume);
		effect.event(document, "keydown", resume);

		effect.spawn(async () => {
			// Register the AudioWorklet processor
			await context.audioWorklet.addModule(RenderWorklet);

			// The context may have been closed while addModule awaited (effect torn down); bail if so.
			// A suspended context is expected here and fine: the worklet is created now and renders
			// once a gesture resumes the context (see the resume handler above).
			if (context.state === "closed") return;

			// Create the worklet node. outputChannelCount must be set explicitly
			// so the process() callback receives a matching channel layout.
			// Firefox defaults differently than Chrome otherwise.
			const worklet = new AudioWorkletNode(context, "render", {
				channelCount,
				channelCountMode: "explicit",
				outputChannelCount: [channelCount],
			});
			effect.cleanup(() => worklet.disconnect());

			// Initial target latency in samples (at the ring's actual rate).
			const latency = this.sync.output.buffer.peek();
			const latencySamples = Math.ceil(ringRate * Time.Second.fromMilli(latency));
			const buffered = this.sync.output.buffered.peek();

			// Let the factory pick the best transport (SharedArrayBuffer or postMessage).
			const ring = createAudioBuffer(worklet, channelCount, ringRate, latencySamples, buffered);
			this.#ring = ring;
			this.#ringSourceRate = sampleRate;
			this.#expectedNext = undefined;
			effect.cleanup(() => {
				ring.close();
				this.#ring = undefined;
				this.#ringSourceRate = undefined;
				this.#expectedNext = undefined;
			});

			// Mirror ring state (timestamp/stalled) onto our public signals.
			effect.run((inner) => {
				const ts = Time.Milli.fromMicro(inner.get(ring.timestamp));
				this.#output.timestamp.set(ts);
				this.#trimDecodeBuffered(ts);
			});
			effect.run((inner) => {
				this.#output.stalled.set(inner.get(ring.stalled));
			});

			effect.set(this.#output.root, worklet);
		});
	}

	#runLatency(effect: Effect): void {
		// Gate on the worklet signal so this effect re-runs once the ring is created.
		const worklet = effect.get(this.#output.root);
		if (!worklet) return;

		const ring = this.#ring;
		if (!ring) return;

		const latency = effect.get(this.sync.output.buffer);
		const latencySamples = Math.ceil(ring.rate * Time.Second.fromMilli(latency));
		ring.setLatency(latencySamples);
	}

	#runDecoder(effect: Effect): void {
		// Re-run (rebuild subscription + decoder) when #onDecoderError bumps #restart.
		effect.get(this.#restart);

		const enabled = effect.get(this.input.enabled);
		if (!enabled) return;

		const broadcast = effect.get(this.source.input.broadcast);
		if (!broadcast) return;

		const track = effect.get(this.source.output.track);
		if (!track) return;

		const config = effect.get(this.source.output.config);
		if (!config) return;

		// A rebuild for a NEW config (not a #restart bump, which keeps the same config object) starts a
		// fresh restart budget. The video decoder gets this free via a per-track instance; here the counter
		// lives on the long-lived Decoder, so reset it explicitly, or an exhausted budget from an old config
		// would kill a healthy new stream on its first transient error.
		if (config !== this.#budgetConfig) {
			this.#budgetConfig = config;
			this.#restartCount = 0;
		}

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source
		// broadcast instead of the catalog's own broadcast.
		const active = broadcast.relativeBroadcast(effect, config.broadcast);
		if (!active) return;

		const sub = active.track(track).subscribe({ priority: Catalog.PRIORITY.audio });
		effect.cleanup(() => sub.close());

		// Both branches below build a fresh container Consumer, whose rewind counter restarts at zero.
		// This field outlives the subscription (unlike video, which keeps it on a per-track object), so a
		// count left over from the previous stream would make the first frame look like a rewind and reset
		// the Sync reference that video shares. Re-anchor the count with the consumer that reports it.
		this.#discontinuity = 0;

		if (config.container.kind === "cmaf") {
			this.#runCmafDecoder(effect, sub, config);
		} else {
			this.#runLegacyDecoder(effect, sub, config);
		}
	}

	#runLegacyDecoder(effect: Effect, sub: Moq.Track.Subscriber, config: Catalog.AudioConfig): void {
		const format = config.container.kind === "loc" ? new Container.Loc.Format() : new Container.Legacy.Format();
		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// TODO include JITTER_UNDERHEAD
		const consumer = new Container.Consumer(sub, {
			format,
			latency: this.#consumerLatency,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#output.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		effect.spawn(async () => {
			const abort = effect.abort; // pin this run's signal; the getter is replaced on every re-run
			const loaded = await Util.Libav.polyfill();
			if (!loaded || abort.aborted) return;

			let warmed = 0;

			const decoder = new AudioDecoder({
				output: (data) => {
					warmed++;
					if (warmed <= 3) {
						// Drop the first 3 frames to prime the decoder.
						data.close();
						return;
					}
					this.#emit(data);
				},
				error: (error) => this.#onDecoderError(error, effect),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
			});

			// Opus in CMAF uses raw packets; dOps is not a valid OGG Identification Header.
			const description =
				config.codec === "opus"
					? undefined
					: config.description
						? Util.Hex.toBytes(config.description)
						: undefined;
			decoder.configure({
				...config,
				description,
			});

			let prevTs: number | undefined;
			for (;;) {
				const next = await consumer.next();
				if (!next) {
					// Track ended cleanly. If our config is unchanged (a deep-equal republish) the effect
					// won't re-run on its own, so resubscribe; a real local teardown aborted before here.
					if (!abort.aborted) this.#onCleanEnd(effect);
					break;
				}

				// Publisher rewound the timeline: flush + re-anchor before decoding the new frame.
				this.#onDiscontinuity(next.discontinuity);

				const { frame } = next;
				if (!frame) continue;

				// A large forward wire gap is a publisher mute/pause: re-anchor to live and skip the wait so
				// the frame re-seeds the (now stalled) ring immediately instead of playing out the silent gap.
				const wireTs = frame.timestamp as number;
				const gapped = prevTs !== undefined && wireTs - prevTs > REANCHOR_GAP_US;
				if (gapped) this.#reanchor();
				prevTs = wireTs;

				// Mark that we received this frame right now.
				const timestamp = Time.Milli.fromMicro(frame.timestamp as Time.Micro);
				this.sync.received(timestamp, "audio");

				this.#output.stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				// Backpressure: in buffered mode this holds the encoded frame until the playhead nears
				// it, keeping the lookahead above the floor as Opus instead of decoded PCM. No-op live.
				if (!gapped) {
					await this.#ring?.wait(frame.timestamp as Time.Micro, abort);
					if (abort.aborted) break;
				}

				const chunk = new EncodedAudioChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.data,
					timestamp: frame.timestamp,
				});

				if (decoder.state === "closed") break;
				try {
					decoder.decode(chunk);
				} catch (err) {
					// A wrong-config chunk makes decode() throw synchronously. Audio frames are independent,
					// so drop the bad one and continue; a closed decoder (from the async error callback) ends
					// the loop and #onDecoderError rebuilds via #restart.
					if (err instanceof DOMException && err.name === "DataError") {
						console.debug("audio decode error; dropping frame", err);
						continue;
					}
					break;
				}
			}
		});
	}

	#runCmafDecoder(effect: Effect, sub: Moq.Track.Subscriber, config: Catalog.AudioConfig): void {
		if (config.container.kind !== "cmaf") return; // just to help typescript

		const initSegment = base64ToBytes(config.container.init);
		const init = Container.Cmaf.decodeInitSegment(initSegment);
		// Opus in CMAF uses raw packets (not OGG-wrapped), so description must be omitted.
		// The dOps box from the init segment is not a valid OGG Identification Header.
		const description =
			config.codec === "opus"
				? undefined
				: config.description
					? Util.Hex.toBytes(config.description)
					: init.description;

		const consumer = new Container.Consumer(sub, {
			format: new Container.Cmaf.Format(init),
			latency: this.#consumerLatency,
		});
		effect.cleanup(() => consumer.close());

		// Combine network jitter buffer with decode buffer
		effect.run((inner) => {
			const network = inner.get(consumer.buffered);
			const decode = inner.get(this.#decodeBuffered);
			this.#output.buffered.update(() => Container.mergeBufferedRanges(network, decode));
		});

		effect.spawn(async () => {
			const abort = effect.abort; // pin this run's signal; the getter is replaced on every re-run
			const loaded = await Util.Libav.polyfill();
			if (!loaded || abort.aborted) return;

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => this.#onDecoderError(error, effect),
			});
			effect.cleanup(() => {
				if (decoder.state !== "closed") decoder.close();
			});

			// Configure decoder with description from catalog
			decoder.configure({
				codec: config.codec,
				sampleRate: config.sampleRate,
				numberOfChannels: config.numberOfChannels,
				description,
			});

			let prevTs: number | undefined;
			for (;;) {
				const next = await consumer.next();
				if (!next) {
					// See #runLegacyDecoder: resubscribe on a clean end unless we tore down locally.
					if (!abort.aborted) this.#onCleanEnd(effect);
					break;
				}

				// Publisher rewound the timeline: flush + re-anchor before decoding the new frame.
				this.#onDiscontinuity(next.discontinuity);

				const { frame } = next;
				if (!frame) continue;

				// A large forward wire gap is a publisher mute/pause: re-anchor to live and skip the wait.
				const wireTs = frame.timestamp as number;
				const gapped = prevTs !== undefined && wireTs - prevTs > REANCHOR_GAP_US;
				if (gapped) this.#reanchor();
				prevTs = wireTs;

				const timestamp = Time.Milli.fromMicro(frame.timestamp);
				this.sync.received(timestamp, "audio");

				this.#output.stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				// Backpressure: in buffered mode this holds the encoded frame until the playhead nears
				// it, keeping the lookahead above the floor as Opus instead of decoded PCM. No-op live.
				if (!gapped) {
					await this.#ring?.wait(frame.timestamp, abort);
					if (abort.aborted) break;
				}

				if (decoder.state === "closed") break;
				try {
					decoder.decode(
						new EncodedAudioChunk({
							type: frame.keyframe ? "key" : "delta",
							data: frame.data,
							timestamp: frame.timestamp,
						}),
					);
				} catch (err) {
					// See #runLegacyDecoder: drop a bad chunk (DataError) and continue; else end the loop.
					if (err instanceof DOMException && err.name === "DataError") {
						console.debug("audio decode error; dropping frame", err);
						continue;
					}
					break;
				}
			}
		});
	}

	// Recover from a fatal AudioDecoder error by rebuilding in place (re-run #runDecoder) instead of
	// leaving the loop abandoned. Capped (reset on a successful #emit); rapid repeats back off. Unlike the
	// video decoder's per-track wrapper, #runDecoder is the persistent effect, so on exhaustion we stop
	// retrying WITHOUT closing it - a later config change re-runs it and a working decoder resets the budget.
	#onDecoderError(error: unknown, effect: Effect): void {
		// If the catalog has already moved past the config this decoder was built for, the error is just
		// wrong-config bytes during a codec/rate switch and #runDecoder is about to rebuild for the new
		// config, so don't burn the restart budget re-decoding stale bytes. Compare fields, not identity:
		// Signal.set stores a deep-equal object without notifying, so peek() can return a new-but-equal one.
		const current = this.source.output.config.peek();
		const budget = this.#budgetConfig;
		if (budget && current && (current.codec !== budget.codec || current.container.kind !== budget.container.kind)) {
			console.debug("audio decoder error; config superseded, rebuild pending", error);
			return;
		}

		if (this.#restartCount >= MAX_AUDIO_RESTARTS) {
			console.error("audio decoder error; giving up until the next config change", error);
			return;
		}
		// Measure "rapid" against when the restart is DISPATCHED (see the video decoder), not the error
		// time, so a backed-off restart doesn't reset the interval and oscillate immediate/backoff.
		const rapid = performance.now() - this.#lastRestart < RESTART_RAPID_MS;
		this.#restartCount++;
		if (this.#restartCount === 1) console.warn("audio decoder error; restarting", error);
		else console.debug("audio decoder error; restarting", error);
		const restart = () => {
			this.#lastRestart = performance.now();
			this.#restart.update((n) => n + 1);
		};
		if (rapid) effect.timer(restart, RESTART_BACKOFF_MS);
		else restart();
	}

	#emit(sample: AudioData) {
		// A frame decoded successfully: reset the restart budget.
		this.#restartCount = 0;

		let timestamp = sample.timestamp as Time.Micro;

		const ring = this.#ring;
		if (!ring) {
			// We're probably in the process of closing.
			sample.close();
			return;
		}

		// If the decoder output an UNEXPECTED rate (not what the pipeline was built for, e.g. HE-AAC
		// emitting 2x the advertised rate), rebuild the pipeline at the real rate and drop until it's up.
		// Compared to the SOURCE rate (not ring.rate) so the deliberate resample-to-context-rate below is
		// not mistaken for a decoder-rate change, which would thrash the rebuild and silence Safari.
		const sourceRate = this.#ringSourceRate;
		if (sourceRate !== undefined && sample.sampleRate !== sourceRate) {
			const config = this.source.output.config.peek();
			const prev = this.#decodedRate.peek();
			// Skip the set when this config already has this rate recorded (the rebuild is just in
			// flight), but always record it for a new config even at a rate a prior stream used.
			if (config && (prev?.config !== config || prev.rate !== sample.sampleRate)) {
				console.warn(`audio decoder outputs ${sample.sampleRate} Hz, expected ${sourceRate} Hz; rebuilding`);
				this.#decodedRate.set({ config, rate: sample.sampleRate });
			}
			sample.close();
			return;
		}

		// Calculate end time from sample duration
		const durationMicro = ((sample.numberOfFrames / sample.sampleRate) * 1_000_000) as Time.Micro;

		// Snap a near-contiguous frame to the previous frame's exact end so the timestamp-indexed ring writes
		// back-to-back instead of zero-filling or overwriting a sample every frame (Safari-to-Safari crackle:
		// Safari's decoder passes the publisher's quantized wire timestamps straight through, where Chrome's
		// regenerates an exact cadence). Magnitude-keyed, not rate-keyed: an already-contiguous stream makes
		// this an identity no-op, so Chrome/Firefox are unaffected. The publisher also snaps at the encoder
		// (see encoder.ts); this covers unfixed publishers. The window is capped at half a frame so a genuine
		// gap (>= one frame: loss, DTX silence, restart) always exceeds it and still zero-fills, whatever the
		// frame duration.
		const snapWindow = Math.min(SNAP_US, durationMicro / 2);
		timestamp = snapTimestamp(this.#expectedNext, timestamp, snapWindow) as Time.Micro;
		this.#expectedNext = (timestamp + durationMicro) as Time.Micro;

		const timestampMilli = Time.Milli.fromMicro(timestamp);
		const durationMilli = Time.Milli.fromMicro(durationMicro);
		const end = Time.Milli.add(timestampMilli, durationMilli);

		// Add to decode buffer
		this.#addDecodeBuffered(timestampMilli, end);

		// Firefox's Opus decoder sometimes outputs more channels than requested
		// (e.g. 6 for stereo). Clamp to the ring's channel count.
		const channels = Math.min(sample.numberOfChannels, ring.channels);
		let channelData: Float32Array[] = [];
		for (let channel = 0; channel < channels; channel++) {
			const data = new Float32Array(sample.numberOfFrames);
			sample.copyTo(data, { format: "f32-planar", planeIndex: channel });
			channelData.push(data);
		}

		// Resample to the ring's rate when the context runs at a different rate than the decoder outputs
		// (Safari pins ~44100 while Opus decodes 48000). outLen comes from the ring's own index rounding so
		// consecutive frames stay exactly contiguous (no zero-fill gap / "floating point inaccuracy" warn).
		// A no-op when the rates match (Chrome/Firefox honor the requested rate).
		if (sample.sampleRate !== ring.rate) {
			const startIndex = Math.round(Time.Second.fromMicro(timestamp) * ring.rate);
			const endMicro = (timestamp + durationMicro) as Time.Micro;
			const outLen = Math.max(0, Math.round(Time.Second.fromMicro(endMicro) * ring.rate) - startIndex);
			channelData = channelData.map((data) => resampleLinear(data, outLen));
		}

		// Hand off to the ring. Shared transport writes directly; post transport
		// transfers the ArrayBuffers.
		ring.insert(timestamp, channelData);

		sample.close();
	}

	#addDecodeBuffered(start: Time.Milli, end: Time.Milli): void {
		if (start > end) return;

		this.#decodeBuffered.mutate((current) => {
			for (const range of current) {
				// Extend range if new sample overlaps or is adjacent (1ms tolerance for float precision)
				if (start <= range.end + 1 && end >= range.start) {
					range.start = Time.Milli.min(range.start, start);
					range.end = Time.Milli.max(range.end, end);
					return;
				}
			}

			current.push({ start, end });
			current.sort((a, b) => a.start - b.start);
		});
	}

	#trimDecodeBuffered(timestamp: Time.Milli): void {
		this.#decodeBuffered.mutate((current) => {
			while (current.length > 0) {
				if (current[0].end >= timestamp) {
					current[0].start = Time.Milli.max(current[0].start, timestamp);
					break;
				}
				current.shift();
			}
		});
	}

	// Flush the audio buffer and re-stall, re-anchoring playback to the next frame. Drops stale buffered
	// PCM WITHOUT touching Sync: a forward gap (mute/pause) keeps the publisher's epoch, and video shares
	// the Sync, so resetting it here would perturb video.
	#reanchor(): void {
		this.#ring?.reset();
		this.#expectedNext = undefined;
	}

	// Public utterance-boundary flush (buffered mode, see Sync.reset).
	reset(): void {
		this.#reanchor();
	}

	// React to the container consumer's discontinuity counter. It changes only on a BACKWARD rewind
	// (publisher timeline reset), so flush the queued PCM and re-anchor the shared clock before the new
	// utterance. Forward gaps (mute/pause) are handled in the decode loop and must NOT reset Sync.
	#onDiscontinuity(count: number): void {
		if (count === this.#discontinuity) return;
		this.#discontinuity = count;
		this.#reanchor();
		this.sync.reset();
	}

	// The publisher closed the audio track but our catalog config is unchanged (a deep-equal republish,
	// e.g. a Sample-rate override that re-pins 48 kHz), so #runDecoder won't re-run on its own and we'd
	// go silent. Resubscribe via the restart budget (a successful #emit resets it); a genuine local
	// teardown aborts the loop before this is reached.
	#onCleanEnd(effect: Effect): void {
		if (this.#restartCount >= MAX_AUDIO_RESTARTS) return;
		this.#restartCount++;
		effect.timer(() => {
			this.#lastRestart = performance.now();
			this.#restart.update((n) => n + 1);
		}, RESTART_BACKOFF_MS);
	}

	close() {
		this.#signals.close();
	}

	// Whether the WebCodecs audio decoder can play this config.
	static supported = supported;
}

// Linear-resample one channel of PCM to `outLen` samples. The caller derives `outLen` from the ring's
// index rounding so consecutive frames stay contiguous; this just maps the input samples across it.
// Bypassed (returns the input) when no rate change is needed.
function resampleLinear(input: Float32Array, outLen: number): Float32Array {
	const inLen = input.length;
	if (outLen === inLen) return input;
	const out = new Float32Array(outLen);
	if (outLen === 0 || inLen === 0) return out;
	if (inLen === 1) {
		out.fill(input[0]);
		return out;
	}
	const step = inLen / outLen;
	for (let j = 0; j < outLen; j++) {
		const pos = j * step;
		const i0 = Math.floor(pos);
		const i1 = Math.min(i0 + 1, inLen - 1);
		const frac = pos - i0;
		out[j] = input[i0] * (1 - frac) + input[i1] * frac;
	}
	return out;
}

async function supported(config: Catalog.AudioConfig): Promise<boolean> {
	// Load the Opus polyfill first. Safari 16.4-18.7 ships no WebCodecs audio API at all, so a bare
	// AudioDecoder.isConfigSupported would throw ReferenceError and drop every rendition. polyfill()
	// returns immediately when a native AudioDecoder already exists (Chrome, Firefox, Safari 26+).
	await Util.Libav.polyfill();

	// Opus in CMAF uses raw packets; dOps is not a valid OGG Identification Header.
	let description: Uint8Array | undefined;
	if (config.codec !== "opus") {
		if (config.description) {
			description = Util.Hex.toBytes(config.description);
		} else if (config.container.kind === "cmaf") {
			try {
				description = Container.Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
			} catch (err) {
				// A malformed init segment means we can't extract the codec
				// description, so we can't probe support reliably. Reject the
				// track rather than letting isConfigSupported pass on a
				// description-less config and then having decode() fail later.
				console.warn(`audio: malformed CMAF init segment for codec ${config.codec}`, err);
				return false;
			}
		}
	}
	const res = await AudioDecoder.isConfigSupported({
		...config,
		description,
	});
	return res.supported ?? false;
}
