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

import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import * as Publish from "@moq/publish";
import { Effect, Signal } from "@moq/signals";
import type { Fault, FixtureState } from "./contract";
import { OFFSET_STEPS } from "./contract";
import * as Pattern from "./pattern";

/** Cap the encoder rather than letting it track a bandwidth estimate, so runs are comparable. */
const MAX_BITRATE = 1_000_000;

/** Short GOP so a late subscriber tunes in quickly and a rejoin is not dominated by keyframe wait. */
const KEYFRAME_INTERVAL = Time.Milli.fromSecond(0.5 as Time.Second);

/** How far ahead the tone table is scheduled on the audio clock. */
const SCHEDULE_AHEAD = 2; // seconds

/** Peak amplitude of the tone. Low enough to leave Opus headroom, high enough to dominate noise. */
const AMPLITUDE = 0.5;

/** Rate the tone is generated and captured at. Stated rather than probed, so the catalog is fixed. */
const SAMPLE_RATE = 48000;

// A canvas capture track is a MediaStreamTrack with requestFrame(); the publish types describe a
// getUserMedia track, whose settings a canvas track does not carry. Only the MediaStreamTrack half
// is ever touched at runtime.
type CanvasSource = Publish.Video.Source & Pick<CanvasCaptureMediaStreamTrack, "requestFrame">;

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
	#timer: number | undefined;

	constructor(host: HTMLElement, url: string, name: string, fault: Fault) {
		this.host = host;

		const canvas = document.createElement("canvas");
		canvas.width = Pattern.WIDTH;
		canvas.height = Pattern.HEIGHT;
		host.appendChild(canvas);

		const ctx = canvas.getContext("2d");
		if (!ctx) throw new Error("2d canvas context unavailable");

		// captureStream(0) never samples on its own: every frame comes from an explicit
		// requestFrame(), which is what ties the emitted frames to the pattern clock.
		const stream = canvas.captureStream(0);
		const videoTrack = stream.getVideoTracks()[0] as unknown as CanvasSource;
		this.#signals.cleanup(() => videoTrack.stop());

		this.#audio = new AudioContext({ sampleRate: SAMPLE_RATE, latencyHint: "interactive" });
		this.#signals.cleanup(() => void this.#audio.close());

		const destination = this.#audio.createMediaStreamDestination();
		const gain = new GainNode(this.#audio, { gain: fault === "silent-audio" ? 0 : AMPLITUDE });
		const oscillator = new OscillatorNode(this.#audio, { type: "sine" });
		oscillator.connect(gain).connect(destination);
		this.#signals.cleanup(() => oscillator.disconnect());

		const audioTrack = destination.stream.getAudioTracks()[0] as unknown as Publish.Audio.StreamTrack;
		this.#signals.cleanup(() => audioTrack.stop());

		const connection = new Moq.Connection.Reload({ url: new URL(url), enabled: true });
		this.#signals.cleanup(() => connection.close());

		const capture = new Publish.Video.Capture({ source: videoTrack });
		this.#signals.cleanup(() => capture.close());

		const broadcast = new Publish.Broadcast({
			connection: connection.established,
			enabled: true,
			name: Moq.Path.from(name),
			display: capture.out.display,
		});
		this.#signals.cleanup(() => broadcast.close());

		const video = new Publish.Video.Encoder("video", {
			broadcast,
			capture,
			enabled: true,
			config: { frameRate: Pattern.FPS, keyframeInterval: KEYFRAME_INTERVAL, maxBitrate: MAX_BITRATE },
		});
		this.#signals.cleanup(() => video.close());

		// A MediaStreamAudioDestinationNode track reports no rate or channel count of its own, and the
		// encoder would otherwise fall back to whatever the graph defaults to.
		const audio = new Publish.Audio.Encoder("audio", {
			broadcast,
			enabled: true,
			source: { track: audioTrack, kind: "music" },
			sampleRate: SAMPLE_RATE,
			channelCount: 1,
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
			this.#runPicture(ctx, videoTrack, start, fault);
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

		this.#signals.run((effect) => {
			const state: FixtureState = {
				frameId: effect.get(this.#frameId),
				audioState: effect.get(this.#audioState),
				ready:
					effect.get(connection.established) !== undefined &&
					effect.get(video.out.catalog) !== undefined &&
					effect.get(audio.out.catalog) !== undefined,
				videoActive: effect.get(video.out.active),
				audioActive: effect.get(audio.out.active),
				encodedFrames: effect.get(video.out.stats).frames,
			};
			host.dataset.smokeFixture = JSON.stringify(state);
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
		this.#signals.interval(schedule, (SCHEDULE_AHEAD * 1000) / 4);
	}

	// Paint and emit one frame per pattern tick. The counter comes from the audio clock rather than
	// the timer, so a late or coalesced tick skips a frame instead of drifting the two apart.
	#runPicture(ctx: CanvasRenderingContext2D, track: CanvasSource, start: number, fault: Fault): void {
		let painted = -1;

		const tick = () => {
			const elapsed = this.#audio.currentTime - start;
			if (elapsed < 0) return;

			const frameId = Math.floor(elapsed * Pattern.FPS);
			if (frameId === painted) return;
			painted = frameId;

			Pattern.paint(ctx, fault === "frozen-video" ? 0 : frameId);
			track.requestFrame();
			this.#frameId.set(frameId);
		};

		// Twice the frame rate: a tick that lands between frames is a no-op, and one that lands late
		// still emits the frame the clock is on.
		this.#timer = self.setInterval(tick, 1000 / Pattern.FPS / 2);
		this.#signals.cleanup(() => {
			if (this.#timer !== undefined) self.clearInterval(this.#timer);
			this.#timer = undefined;
		});
	}

	/** Stop publishing and release the capture, audio graph, and session. */
	close(): void {
		this.#signals.close();
		this.host.replaceChildren();
		delete this.host.dataset.smokeFixture;
	}
}
