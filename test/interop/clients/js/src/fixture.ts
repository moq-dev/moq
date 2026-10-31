/**
 * The deterministic publisher: a self-describing broadcast whose media says what it is.
 *
 * The fake camera is fine for "did any pixel arrive", but it cannot answer "is the picture
 * advancing" or "is the audio the audio that belongs with this picture". This publishes a canvas
 * painting {@link Pattern.paint} and a tone stepping through {@link Pattern.stepFrequency}, both
 * indexed off one `AudioContext` clock, so a subscriber can measure both from its own sinks.
 *
 * Faults are injected here rather than at the subscriber, so the negative controls exercise the
 * same assertions the real cases do.
 *
 * @module
 */

import { Milli, Nano } from "@moq/clock";
import * as Moq from "@moq/net";
import * as Publish from "@moq/publish";
import { Effect, Signal } from "@moq/signals";
import type { Fault, FixtureState } from "./contract";
import { FIXTURE_EVENT, KEYFRAME_INTERVAL_MS, OFFSET_STEPS, SAMPLE_RATE } from "./contract";
import * as Pattern from "./pattern";

/** Cap the encoder rather than letting it track a bandwidth estimate, so runs are comparable. */
const MAX_BITRATE = 1_000_000;

/** Short GOP so a late subscriber tunes in quickly and a rejoin is not dominated by keyframe wait. */
const KEYFRAME_INTERVAL = Milli(KEYFRAME_INTERVAL_MS);

/** How far ahead the tone table is scheduled on the audio clock. */
const SCHEDULE_AHEAD = 2; // seconds

/** Peak amplitude of the tone. Low enough to leave Opus headroom, high enough to dominate noise. */
const AMPLITUDE = 0.5;

/**
 * A running fixture publisher.
 *
 * Construct one per broadcast; {@link close} releases the capture, the audio graph, and the
 * session. The host element carries the state a driver polls.
 */
export class Fixture {
	readonly host: HTMLElement;

	readonly #signals = new Effect();
	readonly #audio: AudioContext;
	readonly #frameId = new Signal(-1);
	readonly #audioState = new Signal<AudioContextState>("suspended");
	readonly #videoEnabled = new Signal(true);
	#timer: number | undefined;
	readonly #canvas: HTMLCanvasElement;
	readonly #video: Publish.Video.Encoder;
	#generation = 0;

	constructor(
		host: HTMLElement,
		{
			url,
			name,
			fault,
			versions,
			generation = 0,
		}: {
			url: string;
			name: string;
			fault: Fault;
			versions?: string[];
			generation?: number;
		},
	) {
		this.#generation = generation;
		this.host = host;

		const canvas = document.createElement("canvas");
		this.#canvas = canvas;
		canvas.width = Pattern.WIDTH;
		canvas.height = Pattern.HEIGHT;
		host.appendChild(canvas);

		const ctx = canvas.getContext("2d");
		if (!ctx) throw new Error("2d canvas context unavailable");

		// captureStream(0) never samples on its own: every frame comes from an explicit
		// requestFrame(), which is what ties the emitted frames to the pattern clock.
		const stream = canvas.captureStream(0);
		// The publish types describe a getUserMedia track, whose settings a canvas track does not carry.
		// Only the MediaStreamTrack half is ever touched at runtime.
		const videoTrack = stream.getVideoTracks()[0] as unknown as Publish.Video.StreamTrack;
		this.#signals.cleanup(() => videoTrack.stop());
		// The spec puts requestFrame() on the track, which Chromium follows; Firefox has it on the
		// stream instead.
		const capturing = stream.getVideoTracks()[0] as Partial<CanvasCaptureMediaStreamTrack>;
		const requestFrame = capturing.requestFrame
			? () => capturing.requestFrame?.()
			: () => (stream as unknown as { requestFrame(): void }).requestFrame();

		this.#audio = new AudioContext({ sampleRate: SAMPLE_RATE, latencyHint: "interactive" });
		this.#signals.cleanup(() => void this.#audio.close());

		const destination = this.#audio.createMediaStreamDestination();
		const gain = new GainNode(this.#audio, { gain: fault === "silent-audio" ? 0 : AMPLITUDE });
		const oscillator = new OscillatorNode(this.#audio, { type: "sine" });
		oscillator.connect(gain).connect(destination);
		this.#signals.cleanup(() => oscillator.disconnect());

		const audioTrack = destination.stream.getAudioTracks()[0] as unknown as Publish.Audio.StreamTrack;
		this.#signals.cleanup(() => audioTrack.stop());

		const connection = new Moq.Connection({ url: new URL(url), enabled: true, versions });
		this.#signals.cleanup(() => connection.close());

		const capture = new Publish.Video.Capture({ source: videoTrack });
		this.#signals.cleanup(() => capture.close());

		const broadcast = new Publish.Broadcast({
			origin: connection.origin,
			enabled: true,
			name: Moq.Path.from(name),
			display: capture.out.display,
		});
		this.#signals.cleanup(() => broadcast.close());

		const video = new Publish.Video.Encoder("video", {
			broadcast,
			capture,
			enabled: this.#videoEnabled,
			config: {
				codec: "avc1",
				frameRate: Pattern.FPS,
				keyframeInterval: KEYFRAME_INTERVAL,
				maxBitrate: MAX_BITRATE,
			},
		});
		this.#video = video;
		this.#signals.cleanup(() => video.close());

		// Mark the codec change only once the probe has published it. Marking on request would paint the
		// new generation into pictures the old codec still encodes onto the old track.
		// Read the catalog before the generation check, so a rerun after marking still tracks a signal.
		this.#signals.run((effect) => {
			const codec = effect.get(video.out.catalog)?.codec;
			if (this.#generation === 0 && codec?.startsWith("vp8")) this.#generation = 1;
		});

		// Handed the source at page load, before any gesture, like a pre-granted microphone. Readiness
		// then requires an audio catalog after the click, which covers the capture resuming its own
		// suspended graph.
		//
		// Forced to mono, so every media check also covers the explicit Web Audio downmix a requested
		// channel count puts in front of the AudioWorklet, the path a macOS microphone takes. The tone
		// is the same in both channels, so the downmix changes nothing a check reads.
		const audioCapture = new Publish.Audio.Capture({
			source: { track: audioTrack, kind: "music" },
			channelCount: 1,
		});
		this.#signals.cleanup(() => audioCapture.close());

		const audio = new Publish.Audio.Encoder("audio", {
			broadcast,
			capture: audioCapture,
			enabled: true,
		});
		this.#signals.cleanup(() => audio.close());

		// The tone and the picture share this clock, so neither can start until the audio graph runs.
		// Without the permissive launch flags that takes a real gesture, and `resume()` on a
		// suspended context simply never settles, so drive it from the state instead of awaiting.
		let started = false;
		const begin = () => {
			if (started || this.#audio.state !== "running") return;
			started = true;

			const start = this.#audio.currentTime + 0.2;
			oscillator.start(start);
			this.#runTone(oscillator, start, fault);
			this.#runPicture(ctx, requestFrame, start, fault);
		};
		const unlock = () => void this.#audio.resume().catch(() => {});

		this.#audioState.set(this.#audio.state);
		this.#signals.event(this.#audio, "statechange", () => {
			this.#audioState.set(this.#audio.state);
			begin();
		});
		this.#signals.event(document, "pointerdown", unlock);
		this.#signals.event(document, "keydown", unlock);
		unlock();
		begin();

		let readyAt: number | undefined;
		this.#signals.run((effect) => {
			const ready =
				effect.get(connection.status) === "connected" &&
				effect.get(video.out.catalog) !== undefined &&
				effect.get(audio.out.catalog) !== undefined;
			if (ready) readyAt ??= Date.now();
			const state: FixtureState = {
				frameId: effect.get(this.#frameId),
				audioState: effect.get(this.#audioState),
				ready,
				readyAt,
				videoActive: effect.get(video.out.active),
				audioActive: effect.get(audio.out.active),
				encodedFrames: effect.get(video.out.stats).frames,
				videoEnabled: effect.get(video.out.catalog)?.enabled !== false,
			};
			console.debug(FIXTURE_EVENT + JSON.stringify(state));
		});
	}

	// Schedule the tone table on the audio clock, always a couple of seconds ahead of playout.
	#runTone(oscillator: OscillatorNode, start: number, fault: Fault): void {
		const offset = fault === "audio-offset" ? OFFSET_STEPS : 0;
		let step = 0;

		const schedule = () => {
			const horizon = this.#audio.currentTime + SCHEDULE_AHEAD;
			while (start + (step * Pattern.STEP_MS) / 1000 < horizon) {
				const at = start + (step * Pattern.STEP_MS) / 1000;
				oscillator.frequency.setValueAtTime(Pattern.stepFrequency(step + offset), at);
				step++;
			}
		};

		schedule();
		this.#signals.interval(schedule, Nano.fromMilli(Milli((SCHEDULE_AHEAD * 1000) / 4)));
	}

	// Paint and request capture per pattern tick. Capture and encoding can finish later. The counter
	// comes from the audio clock, so a coalesced tick skips a frame instead of drifting the two apart.
	#runPicture(ctx: CanvasRenderingContext2D, requestFrame: () => void, start: number, fault: Fault): void {
		let painted = -1;

		const tick = () => {
			const elapsed = this.#audio.currentTime - start;
			if (elapsed < 0) return;

			const frameId = Math.floor(elapsed * Pattern.FPS);
			if (frameId === painted) return;
			painted = frameId;

			Pattern.paint(ctx, fault === "frozen-video" ? 0 : frameId, this.#generation);
			requestFrame();
			this.#frameId.set(frameId);
		};

		// Twice the frame rate: a tick that lands between frames is a no-op, and one that lands late
		// still requests capture of the frame the clock is on.
		this.#timer = self.setInterval(tick, 1000 / Pattern.FPS / 2);
		this.#signals.cleanup(() => {
			if (this.#timer !== undefined) self.clearInterval(this.#timer);
			this.#timer = undefined;
		});
	}

	/** Enable or disable the video rendition, which stays in the catalog either way. */
	setVideo(enabled: boolean): void {
		this.#videoEnabled.set(enabled);
	}

	/** Switch codecs on the existing rendition, marking pictures once the catalog advertises VP8. */
	changeVideoCodec(): void {
		this.#video.config.update((config) => ({ ...config, codec: "vp8" }));
	}

	/** Grow past the accepted coded-size ceiling without changing the VP8 codec. */
	resizeVideo(): void {
		this.#generation = 2;
		this.#canvas.width = Pattern.WIDTH * 2;
		this.#canvas.height = Pattern.HEIGHT * 2;
	}

	/** Stop publishing and release the capture, audio graph, and session. */
	close(): void {
		this.#signals.close();
		this.host.replaceChildren();
		console.debug(`${FIXTURE_EVENT}null`);
	}
}
