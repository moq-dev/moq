import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import * as Frame from "../../frame";
import type * as Time from "../../time";
import * as Hex from "../../util/hex";
import * as libav from "../../util/libav";
import type { SourceMSE } from "../source-mse";
import type * as Video from "../video";
import type * as Render from "./render";

// We want some extra overhead to avoid starving the render worklet.
// The default Opus frame duration is 20ms.
// TODO: Put it in the catalog so we don't have to guess.
const JITTER_UNDERHEAD = 25 as Time.Milli;

export type SourceProps = {
	// Enable to download the audio track.
	enabled?: boolean | Signal<boolean>;

	// Jitter buffer size in milliseconds (default: 100ms)
	latency?: Time.Milli | Signal<Time.Milli>;
};

export interface AudioStats {
	bytesReceived: number;
}

// Unfortunately, we need to use a Vite-exclusive import for now.
import RenderWorklet from "./render-worklet.ts?worker&url";

// Downloads audio from a track and emits it to an AudioContext.
// The user is responsible for hooking up audio to speakers, an analyzer, etc.
export class Source {
	broadcast: Getter<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;

	#context = new Signal<AudioContext | undefined>(undefined);
	readonly context: Getter<AudioContext | undefined> = this.#context;

	// The root of the audio graph, which can be used for custom visualizations.
	#worklet = new Signal<AudioWorkletNode | undefined>(undefined);
	// Downcast to AudioNode so it matches Publish.Audio
	readonly root = this.#worklet as Getter<AudioNode | undefined>;

	// For MSE path, expose the HTMLAudioElement for direct control
	#mseAudioElement = new Signal<HTMLAudioElement | undefined>(undefined);
	readonly mseAudioElement = this.#mseAudioElement as Getter<HTMLAudioElement | undefined>;

	#sampleRate = new Signal<number | undefined>(undefined);
	readonly sampleRate: Getter<number | undefined> = this.#sampleRate;

	#stats = new Signal<AudioStats | undefined>(undefined);
	readonly stats: Getter<AudioStats | undefined> = this.#stats;

	catalog = new Signal<Catalog.Audio | undefined>(undefined);
	config = new Signal<Catalog.AudioConfig | undefined>(undefined);

	// Not a signal because I'm lazy.
	readonly latency: Signal<Time.Milli>;

	// The name of the active rendition.
	active = new Signal<string | undefined>(undefined);

	#signals = new Effect();

	// Reference to video source for coordination
	video?: Video.Source;

	constructor(
		broadcast: Getter<Moq.Broadcast | undefined>,
		catalog: Getter<Catalog.Root | undefined>,
		props?: SourceProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);
		this.latency = Signal.from(props?.latency ?? (100 as Time.Milli)); // TODO Reduce this once fMP4 stuttering is fixed.

		this.#signals.effect((effect) => {
			const audio = effect.get(catalog)?.audio;
			this.catalog.set(audio);

			if (audio?.renditions) {
				const first = Object.entries(audio.renditions).at(0);
				if (first) {
					effect.set(this.active, first[0]);
					effect.set(this.config, first[1]);
				}
			}
		});

		this.#signals.effect(this.#runWorklet.bind(this));
		this.#signals.effect(this.#runEnabled.bind(this));
		this.#signals.effect(this.#runDecoder.bind(this));
	}

	#runWorklet(effect: Effect): void {
		// It takes a second or so to initialize the AudioContext/AudioWorklet, so do it even if disabled.
		// This is less efficient for video-only playback but makes muting/unmuting instant.

		//const enabled = effect.get(this.enabled);
		//if (!enabled) return;

		const config = effect.get(this.config);
		if (!config) return;

		// Don't create worklet for MSE (cmaf) - browser handles playback directly
		// The worklet is only needed for WebCodecs path
		if (config.container === "cmaf") {
			return;
		}

		const sampleRate = config.sampleRate;
		const channelCount = config.numberOfChannels;

		// NOTE: We still create an AudioContext even when muted.
		// This way we can process the audio for visualizations.

		const context = new AudioContext({
			latencyHint: "interactive", // We don't use real-time because of the jitter buffer.
			sampleRate,
		});
		effect.set(this.#context, context);

		effect.cleanup(() => context.close());

		effect.spawn(async () => {
			// Register the AudioWorklet processor
			await context.audioWorklet.addModule(RenderWorklet);

			// Ensure the context is running before creating the worklet
			if (context.state === "closed") return;

			// Create the worklet node
			const worklet = new AudioWorkletNode(context, "render", {
				channelCount,
				channelCountMode: "explicit",
			});
			effect.cleanup(() => worklet.disconnect());

			const init: Render.Init = {
				type: "init",
				rate: sampleRate,
				channels: channelCount,
				latency: this.latency.peek(), // TODO make it reactive
			};
			worklet.port.postMessage(init);

			effect.set(this.#worklet, worklet);
		});
	}

	#runEnabled(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const context = effect.get(this.#context);
		if (!context) return;

		context.resume();

		// NOTE: You should disconnect/reconnect the worklet to save power when disabled.
	}

	#runDecoder(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		const config = effect.get(this.config);

		// For CMAF, we need to add the SourceBuffer even if audio is disabled
		// This ensures the MediaSource has both SourceBuffers before video starts appending
		// We'll just not append audio data if disabled
		if (config?.container === "cmaf") {
			// Always initialize MSE for CMAF, even if disabled
			// The SourceBuffer needs to be added before video starts appending
		} else if (!enabled) {
			// For non-CMAF, if disabled, don't initialize
			return;
		}

		if (!enabled && config?.container !== "cmaf") {
			return;
		}

		const catalog = effect.get(this.catalog);
		if (!catalog) {
			return;
		}

		const broadcast = effect.get(this.broadcast);
		if (!broadcast) {
			return;
		}

		if (!config) {
			return;
		}

		const active = effect.get(this.active);
		if (!active) {
			return;
		}

		// Route to MSE for CMAF, WebCodecs for native/raw
		// For CMAF, ALWAYS initialize MSE (even if disabled) to add SourceBuffer
		// This ensures MediaSource has both SourceBuffers before video starts appending
		// The SourceBuffer will be added, but fragments won't be appended if disabled
		console.log(`[Audio Source] Routing audio: container=${config.container}, enabled=${enabled}`);
		if (config.container === "cmaf") {
			// Always initialize for CMAF - SourceBuffer must be added before video starts
			console.log("[Audio Source] Using MSE path for CMAF");
			this.#runMSEPath(effect, broadcast, active, config, catalog);
		} else {
			// For non-CMAF, only run if enabled
			console.log(`[Audio Source] Using WebCodecs path (container=${config.container})`);
			if (enabled) {
				this.#runWebCodecsPath(effect, broadcast, active, config, catalog);
			}
		}
	}

	#runMSEPath(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: Catalog.AudioConfig,
		catalog: Catalog.Audio,
	): void {
		// Use the unified SourceMSE from video - it manages both video and audio SourceBuffers
		// Use a reactive effect to always get the latest SourceMSE instance
		effect.spawn(async () => {
			// Wait for video's MSE source to be available
			// Video creates it asynchronously, and may recreate it when restarting
			// So we need to get it reactively each time
			let videoMseSource: SourceMSE | undefined;
			if (this.video?.mseSource) {
				// Wait up to 2 seconds for video MSE source to be available
				const maxWait = 2000;
				const startTime = Date.now();
				while (!videoMseSource && Date.now() - startTime < maxWait) {
					videoMseSource = effect.get(this.video.mseSource);
					if (!videoMseSource) {
						await new Promise((resolve) => setTimeout(resolve, 50)); // Check more frequently
					}
				}
			}

			if (!videoMseSource) {
				console.error("[Audio Source] Video MSE source not available, falling back to WebCodecs");
				this.#runWebCodecsPath(effect, broadcast, name, config, catalog);
				return;
			}

			// For MSE path, audio plays through the video element
			// Expose video element as "audioElement" for compatibility with emitter
			// Use reactive effect to always get the latest video element
			this.#signals.effect((eff) => {
				// Get latest SourceMSE instance in case video restarted
				const latestMseSource = this.video?.mseSource ? eff.get(this.video.mseSource) : undefined;
				const mseSource = latestMseSource || videoMseSource;
				const videoElement = mseSource?.videoElement ? eff.get(mseSource.videoElement) : undefined;
				// Expose as audioElement for emitter compatibility (HTMLVideoElement works the same as HTMLAudioElement for volume/mute)
				eff.set(this.#mseAudioElement, videoElement as HTMLAudioElement | undefined);
			});

			// Forward stats (audio stats are not currently tracked in unified SourceMSE, but we can add them later)
			// For now, just set empty stats
			this.#signals.effect((eff) => {
				eff.set(this.#stats, { bytesReceived: 0 });
			});

			// Check if audio is enabled
			const isEnabled = effect.get(this.enabled);

			// Only subscribe to track and initialize SourceBuffer if enabled
			// When disabled, we don't need to do anything - video can play without audio
			if (!isEnabled) {
				console.log(
					`[Audio Source] Audio disabled, skipping SourceBuffer initialization and track subscription - video will play without audio`,
				);
				return;
			}

			// Audio is enabled - subscribe to track and initialize SourceBuffer
			// Wait a bit for video to stabilize if it's restarting
			// Get the latest SourceMSE instance and verify it's stable
			let latestMseSource: SourceMSE | undefined;
			let retryCount = 0;
			const maxRetries = 3;

			while (retryCount < maxRetries) {
				// Get the latest SourceMSE instance (in case video restarted)
				latestMseSource = this.video?.mseSource ? effect.get(this.video.mseSource) : videoMseSource;
				if (!latestMseSource) {
					// Wait a bit for video to create SourceMSE
					await new Promise((resolve) => setTimeout(resolve, 100));
					retryCount++;
					continue;
				}

				// Check if MediaSource is ready (not closed)
				const mediaSource = latestMseSource.mediaSource ? effect.get(latestMseSource.mediaSource) : undefined;
				if (
					mediaSource &&
					typeof mediaSource === "object" &&
					"readyState" in mediaSource &&
					(mediaSource as MediaSource).readyState === "closed"
				) {
					// MediaSource is closed, video might be restarting - wait and retry
					console.log("[Audio Source] MediaSource is closed, waiting for video to stabilize");
					await new Promise((resolve) => setTimeout(resolve, 200));
					retryCount++;
					continue;
				}

				// SourceMSE instance looks good, proceed
				break;
			}

			if (!latestMseSource) {
				console.warn("[Audio Source] SourceMSE instance not available after retries, skipping audio");
				return;
			}

			console.log("[Audio Stream] Subscribing to track", {
				name,
				codec: config.codec,
				container: config.container,
				sampleRate: config.sampleRate,
				channels: config.numberOfChannels,
			});

			// Retry a few times for transient MSE states / QuotaExceeded
			for (let attempt = 0; attempt < 5; attempt++) {
				try {
					// Resolve freshest SourceMSE and wait for MediaSource to be open (up to ~5s).
					const resolveOpenMediaSource = async (): Promise<SourceMSE> => {
						const start = Date.now();
						let current = latestMseSource;
						for (;;) {
							// Follow any video restart by re-reading the signal
							const candidate = this.video?.mseSource ? effect.get(this.video.mseSource) : current;
							if (candidate && candidate !== current) {
								console.log("[Audio Source] Video restarted, using new SourceMSE instance");
								current = candidate;
							}

							if (!current) {
								if (Date.now() - start > 5000) {
									throw new Error("SourceMSE not available");
								}
								await new Promise((resolve) => setTimeout(resolve, 50));
								continue;
							}

							const ms = current.mediaSource ? effect.get(current.mediaSource) : undefined;
							if (
								ms &&
								typeof ms === "object" &&
								"readyState" in ms &&
								(ms as MediaSource).readyState === "open"
							) {
								return current;
							}

							if (Date.now() - start > 5000) {
								throw new Error("MediaSource not ready for audio SourceBuffer");
							}
							await new Promise((resolve) => setTimeout(resolve, 50));
						}
					};

					const readyMseSource = await resolveOpenMediaSource();
					latestMseSource = readyMseSource;

					console.log(
						`[Audio Source] Initializing audio SourceBuffer on unified SourceMSE (attempt ${attempt + 1})`,
					);
					await latestMseSource.initializeAudio(config);

					// Verify we're still using the current instance after initialization
					const verifyMseSource = this.video?.mseSource ? effect.get(this.video.mseSource) : latestMseSource;
					if (verifyMseSource && verifyMseSource !== latestMseSource) {
						// Video restarted during initialization, get new instance and retry
						console.log("[Audio Source] Video restarted during initialization, retrying with new instance");
						await verifyMseSource.initializeAudio(config);
						latestMseSource = verifyMseSource;
					}

					console.log(`[Audio Source] Audio SourceBuffer initialization completed`);

					// Get latest instance again before running track (video might have restarted)
					const finalMseSource = this.video?.mseSource ? effect.get(this.video.mseSource) : latestMseSource;
					if (!finalMseSource) {
						throw new Error("SourceMSE instance not available");
					}

					// Run audio track - use the latest instance
					console.log(`[Audio Source] Starting MSE track on unified SourceMSE`);
					await finalMseSource.runAudioTrack(effect, broadcast, name, config, catalog, this.enabled);
					console.log("[Audio Source] MSE track completed successfully");
					return; // success
				} catch (error) {
					const retriable = error instanceof DOMException && error.name === "QuotaExceededError";
					if (!retriable || attempt === 4) {
						console.warn(
							"[Audio Source] Failed to initialize audio SourceBuffer, video will continue without audio:",
							error,
						);
						return;
					}
					const delay = 150 + attempt * 150;
					console.warn(
						`[Audio Source] Audio init attempt ${attempt + 1} failed (${(error as Error).message}); retrying in ${delay}ms`,
					);
					await new Promise((resolve) => setTimeout(resolve, delay));
				}
			}
		});
	}

	#runWebCodecsPath(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: Catalog.AudioConfig,
		catalog: Catalog.Audio,
	): void {
		console.log("[Audio Stream] Subscribing to track", {
			name,
			codec: config.codec,
			container: config.container,
			sampleRate: config.sampleRate,
			channels: config.numberOfChannels,
		});
		const sub = broadcast.subscribe(name, catalog.priority);
		effect.cleanup(() => sub.close());

		// Create consumer with slightly less latency than the render worklet to avoid underflowing.
		// Container defaults to "native" via Zod schema for backward compatibility
		const consumer = new Frame.Consumer(sub, {
			latency: Math.max(this.latency.peek() - JITTER_UNDERHEAD, 0) as Time.Milli,
			container: config.container,
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			const loaded = await libav.polyfill();
			if (!loaded) return; // cancelled

			const decoder = new AudioDecoder({
				output: (data) => this.#emit(data),
				error: (error) => console.error(error),
			});
			effect.cleanup(() => decoder.close());

			const description = config.description ? Hex.toBytes(config.description) : undefined;
			decoder.configure({
				...config,
				description,
			});

			for (;;) {
				const frame = await consumer.decode();
				if (!frame) break;

				this.#stats.update((stats) => ({
					bytesReceived: (stats?.bytesReceived ?? 0) + frame.data.byteLength,
				}));

				const chunk = new EncodedAudioChunk({
					type: frame.keyframe ? "key" : "delta",
					data: frame.data,
					timestamp: frame.timestamp,
				});

				decoder.decode(chunk);
			}
		});
	}

	#emit(sample: AudioData) {
		const timestamp = sample.timestamp as Time.Micro;

		const worklet = this.#worklet.peek();
		if (!worklet) {
			// We're probably in the process of closing.
			sample.close();
			return;
		}

		const channelData: Float32Array[] = [];
		for (let channel = 0; channel < sample.numberOfChannels; channel++) {
			const data = new Float32Array(sample.numberOfFrames);
			sample.copyTo(data, { format: "f32-planar", planeIndex: channel });
			channelData.push(data);
		}

		const msg: Render.Data = {
			type: "data",
			data: channelData,
			timestamp,
		};

		// Send audio data to worklet via postMessage
		// TODO: At some point, use SharedArrayBuffer to avoid dropping samples.
		worklet.port.postMessage(
			msg,
			msg.data.map((data) => data.buffer),
		);

		sample.close();
	}

	close() {
		this.#signals.close();
	}
}
